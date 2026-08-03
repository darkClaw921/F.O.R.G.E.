//! LongPoll-воркер: единственный источник realtime у плагина VK.
//!
//! ```text
//!  supervisor ──► wait_authorized ──► obtain_cursor ──► run_worker
//!       ▲               ▲                   │              │
//!       │        authorized_notify          │              ▼
//!       └────── backoff_sleep ◄─────────────┴──── a_check → parse_poll
//!                                                            │
//!                       broadcast(VkServerMsg) ◄── map_event ┘
//! ```
//!
//! ## Что здесь принципиально отличается от телеграмного `updates.rs`
//!
//! У Telegram есть постоянный MTProto-сокет и `catch_up`, который догоняет
//! пропущенное за офлайн-период по `pts`. **У VK прямого аналога нет**, и
//! изображать его нельзя: `messages.getLongPollHistory` с `ts` старше суток
//! отвечает кодом 10. Поэтому на старте берётся СВЕЖИЙ `ts` и поток идёт «с
//! сейчас», а история беседы подтягивается обычным `messages.getHistory` при
//! её открытии. Сохранённый курсор нужен только для тёплого догона внутри
//! суток (перезапуск процесса, обрыв связи) — см. [`CURSOR_MAX_AGE_SECS`].
//!
//! ## Обогащение пачки: ОДИН запрос, а не запрос на сообщение
//!
//! Событие 4 (новое сообщение) приезжает урезанным. Рассматривались три пути:
//!
//! 1. **Разбор плоских полей `attach1_type`/`attach1`** — отвергнут. Это
//!    второй параллельный парсер второго недокументированного формата, и он
//!    физически не отдаёт `fwd_messages`, `reply_message`, `action`,
//!    `keyboard`. Кадр `new_message` начал бы расходиться по полноте с
//!    `GET /messages`, и фронт получал бы «мигающие» карточки при reconcile.
//! 2. **`messages.getById` на каждое событие** — отвергнут. Цена — один
//!    запрос на СООБЩЕНИЕ. При бюджете 3 rps залп из десяти сообщений в
//!    активной беседе мгновенно съедает три секунды бюджета, начинает
//!    генерировать код 6, а троттлер принимается задерживать интерактивные
//!    запросы пользователя ради фоновой доставки.
//! 3. **`messages.getLongPollHistory(ts, pts)` — ВЫБРАН.** Один запрос на
//!    ПАЧКУ независимо от числа событий; отдаёт полноценные объекты сообщений
//!    (те же, что `getHistory`) плюс `profiles`/`groups` — заодно бесплатно
//!    закрывает [`crate::peers`]. `new_pts` становится курсором.
//!
//! Если событий 4/5/18 в пачке нет, дозапроса нет ВОВСЕ: typing, online и
//! отметки прочтения стоят ноль запросов. Единственное место, где допустим
//! путь (2), — деградация при отказе `getLongPollHistory`, и там он ограничен
//! [`MAX_FALLBACK_GETBYID`] сообщениями, а остаток закрывается кадром
//! [`VkServerMsg::Resync`].
//!
//! ## Два обязательных условия, без которых обогащение не работает
//!
//! `pts` приходит ТОЛЬКО при `messages.getLongPollServer(need_pts=1)` И
//! бите 32 в [`LP_MODE`]. Это не две независимые опции, а одна пара: без неё
//! `getLongPollHistory` вызвать нечем, и остаётся только отвергнутый путь (2).
//!
//! ## Почему тайм-аут `a_check` больше дефолтного
//!
//! Штатное ожидание на LongPoll-сервере — [`LP_WAIT_SECS`] секунд. Дефолтные
//! 30 секунд `reqwest` зарезали бы его на границе, поэтому у `a_check` свой
//! тайм-аут `wait + `[`crate::api::LONGPOLL_TIMEOUT_SLACK`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::error::{CODE_ACCESS_DENIED, CODE_AUTH_FAILED, CODE_CAPTCHA, CODE_INTERNAL_SERVER};
use crate::api::schema::LongPollHistoryRaw;
use crate::api::{VkClient, VkError};
use crate::dto::{dialog_id_string, MessageDto, PeerNames, CHAT_PEER_BASE};
use crate::state::{LpCursor, VkState};
use crate::ws::protocol::VkServerMsg;

// ---------------------------------------------------------------------------
// Константы.
// ---------------------------------------------------------------------------

/// Лестница пауз между попытками, секунды. Последняя ступень повторяется.
///
/// Дословно телеграмная: одинаковое поведение двух панелей при недоступном
/// сервере — вопрос предсказуемости для пользователя, а не эстетики.
pub const BACKOFF_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// Как часто воркер перепроверяет авторизацию, если его не разбудили.
///
/// Основной путь пробуждения — [`VkState::authorized_notify`]; тик существует
/// как страховка от пропущенного уведомления и стоит ноль запросов к VK.
pub const AUTH_POLL: Duration = Duration::from_secs(2);

/// Штатное ожидание событий на LongPoll-сервере, секунды.
pub const LP_WAIT_SECS: u64 = 25;

/// Версия протокола LongPoll (числовой близнец [`crate::api::methods::LP_VERSION`]).
pub const LP_VERSION: u32 = 3;

/// Режим LongPoll: 2 (текст) | 8 (расширенные) | 32 (pts) | 64 (платформа) |
/// 128 (`random_id`) = 234.
///
/// Бит 32 обязателен: без него в ответах `a_check` не будет `pts`, и
/// обогащение пачки одним запросом станет невозможным (см. шапку модуля).
pub const LP_MODE: u32 = 2 | 8 | 32 | 64 | 128;

/// Сколько сообщений разрешено добрать `messages.getById`, если обогащение
/// пачки отказало.
///
/// Три — это компромисс: типичный залп в беседе больше, но три запроса ещё
/// укладываются в одну секунду бюджета. Всё, что сверх, закрывается кадром
/// [`VkServerMsg::Resync`]: перечитать состояние по REST дешевле, чем
/// доставить каждое сообщение отдельным запросом.
pub const MAX_FALLBACK_GETBYID: usize = 3;

/// Имя файла курсора в `cache_dir`.
pub const CURSOR_FILE: &str = "longpoll.json";

/// Через сколько успешных циклов курсор сбрасывается на диск.
///
/// Не каждый цикл: пачка событий приходит в активной беседе по несколько раз в
/// секунду, и запись файла на каждую означала бы `fsync` в горячем пути.
/// Основная запись — на выходе воркера, эта — страховка от `SIGKILL`.
pub const CURSOR_PERSIST_EVERY: u32 = 20;

/// Сколько живёт сохранённый курсор, секунды.
///
/// `messages.getLongPollHistory` с `ts` старше суток отвечает кодом 10, то
/// есть протухший курсор — это не «догон подлиннее», а гарантированный отказ
/// на каждом цикле. Такой курсор выбрасывается ещё при загрузке, и поток
/// начинается «с сейчас».
pub const CURSOR_MAX_AGE_SECS: i64 = 24 * 60 * 60;

/// Бит «не прочитано» в маске флагов сообщения.
pub const FLAG_UNREAD: i64 = 1;

/// Бит «исходящее».
pub const FLAG_OUTBOX: i64 = 2;

/// Бит «удалено».
pub const FLAG_DELETED: i64 = 128;

/// Действие в кадре [`VkServerMsg::Typing`] — «печатает».
pub const ACTION_TYPING: &str = "typing";

/// Действие в кадре [`VkServerMsg::Typing`] — «записывает голосовое».
pub const ACTION_RECORD_VOICE: &str = "record_voice";

/// Направление в кадре [`VkServerMsg::Read`] — «прочитали мы».
pub const DIRECTION_IN: &str = "in";

/// Направление в кадре [`VkServerMsg::Read`] — «прочитали нас».
pub const DIRECTION_OUT: &str = "out";

// ---------------------------------------------------------------------------
// Тайминги (данные, а не константы).
// ---------------------------------------------------------------------------

/// Тайминги воркера, вынесенные в состояние.
///
/// Ровно по той же причине, что тайминги WS в
/// [`VkState::with_ws_timings`]: иначе тест «после `failed:2` воркер не потерял
/// ни одного кадра» обязан был бы спать боевую секунду backoff'а, а тест
/// «вставка токена поднимает воркера» — две секунды тика. Продовый путь всегда
/// идёт через [`VkState::new`], где стоят боевые значения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpTimings {
    /// Период перепроверки авторизации.
    pub auth_poll: Duration,
    /// Лестница пауз между попытками.
    pub backoff: Vec<Duration>,
    /// Сколько секунд держать `a_check`.
    pub wait_secs: u64,
    /// Через сколько успешных циклов писать курсор на диск.
    pub persist_every: u32,
}

impl Default for LpTimings {
    fn default() -> Self {
        Self {
            auth_poll: AUTH_POLL,
            backoff: BACKOFF_SECS.iter().map(|s| Duration::from_secs(*s)).collect(),
            wait_secs: LP_WAIT_SECS,
            persist_every: CURSOR_PERSIST_EVERY,
        }
    }
}

impl LpTimings {
    /// Боевые тайминги с `wait_secs` из конфига.
    ///
    /// `longpoll_wait_secs` — единственный тайминг воркера, который
    /// пользователь вправе менять: за длину ожидания на чужом сервере иногда
    /// приходится платить прокси-серверам.
    pub fn from_config(cfg: &crate::config::VkConfig) -> Self {
        Self {
            wait_secs: cfg.longpoll_wait_secs.max(1),
            ..Self::default()
        }
    }

    /// Пауза для ступени `step` (последняя ступень повторяется).
    pub fn backoff_at(&self, step: usize) -> Duration {
        if self.backoff.is_empty() {
            return Duration::from_secs(1);
        }
        let idx = step.min(self.backoff.len() - 1);
        self.backoff[idx]
    }

    /// Индекс последней ступени лестницы.
    pub fn last_step(&self) -> usize {
        self.backoff.len().saturating_sub(1)
    }
}

// ---------------------------------------------------------------------------
// Чистое ядро: разбор ответа a_check.
// ---------------------------------------------------------------------------

/// Разобранный ответ LongPoll-сервера.
#[derive(Debug, Clone, PartialEq)]
pub enum LpPoll {
    /// Пачка событий.
    Updates {
        /// Новая позиция во времени.
        ts: u64,
        /// Новая позиция в истории (приходит только при бите 32 в [`LP_MODE`]).
        pts: Option<u64>,
        /// Сами события: каждое — МАССИВ разнородных значений.
        updates: Vec<Vec<Value>>,
    },
    /// Протокольный отказ (`{"failed": N}`).
    Failed(LpFailed),
    /// Ответ, который вообще не похож на протокол LongPoll.
    Malformed(String),
}

/// Виды `failed` из протокола LongPoll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LpFailed {
    /// `failed:1` — устарел `ts`, свежий приехал в том же ответе.
    StaleTs {
        /// `ts` из ответа.
        new_ts: u64,
    },
    /// `failed:2` — истёк `key`.
    ExpiredKey,
    /// `failed:3` — информация потеряна, нужны и `key`, и `ts`.
    InfoLost {
        /// `ts` из ответа, если он там был. НЕ используется при
        /// восстановлении — см. [`recovery_for`].
        new_ts: Option<u64>,
    },
}

/// Что делать после `failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LpRecovery {
    /// Взять `ts` из ответа, ключ оставить прежним.
    UseTs(u64),
    /// Перезапросить `getLongPollServer`, но `ts` СВОЙ сохранить.
    RenewKeyKeepTs,
    /// Перезапросить всё и сбросить курсор.
    RenewAll,
}

/// Разбирает тело ответа `act=a_check`.
///
/// Три исхода и ни одного четвёртого: либо пачка событий, либо протокольный
/// `failed`, либо мусор. Пустой `updates` — это ПАЧКА (штатный выход по
/// тайм-ауту ожидания), а не ошибка.
pub fn parse_poll(body: &Value) -> LpPoll {
    if let Some(code) = body.get("failed").and_then(Value::as_i64) {
        let ts = body.get("ts").and_then(Value::as_u64);
        return match code {
            1 => match ts {
                Some(new_ts) => LpPoll::Failed(LpFailed::StaleTs { new_ts }),
                // `failed:1` без `ts` — нарушение протокола: сервер сказал
                // «твой ts устарел», но не сказал, куда встать. Молча
                // подставить ноль нельзя (для VK это «с начала времён»),
                // оставить свой — тоже (он заведомо устарел). Честный выход —
                // объявить ответ непонятным и пересобрать курсор целиком.
                None => LpPoll::Malformed("failed:1 without ts".to_string()),
            },
            2 => LpPoll::Failed(LpFailed::ExpiredKey),
            3 => LpPoll::Failed(LpFailed::InfoLost { new_ts: ts }),
            other => LpPoll::Malformed(format!("unknown failed code {other}")),
        };
    }

    let Some(ts) = body.get("ts").and_then(as_u64_lenient) else {
        return LpPoll::Malformed("response has neither `failed` nor `ts`".to_string());
    };
    let updates = match body.get("updates") {
        // `updates` может отсутствовать вовсе — VK так отвечает на первый
        // запрос с только что полученным `ts`.
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Array(fields) => fields.clone(),
                // Событие обязано быть массивом. Объект на этом месте — это
                // либо новая версия протокола, либо чужой ответ; и то и другое
                // безопаснее пропустить пустым событием, чем уронить цикл.
                other => vec![other.clone()],
            })
            .collect(),
        Some(_) => return LpPoll::Malformed("`updates` is not an array".to_string()),
    };

    LpPoll::Updates {
        ts,
        pts: body.get("pts").and_then(as_u64_lenient),
        updates,
    }
}

/// Действие по протоколу `failed` — дословно.
///
/// `failed:2` НЕ берёт `ts` из ответа намеренно: пока ключ протухал, на
/// сервере копились события, и `ts` из свежего `getLongPollServer` указывает
/// на «сейчас». Взяв его, мы бы молча потеряли всё, что пришло за время
/// отсутствия, — и именно этот случай проверяет интеграционный тест
/// «после `failed:2` не потеряно ни одного кадра».
pub fn recovery_for(f: &LpFailed) -> LpRecovery {
    match f {
        LpFailed::StaleTs { new_ts } => LpRecovery::UseTs(*new_ts),
        LpFailed::ExpiredKey => LpRecovery::RenewKeyKeepTs,
        // `new_ts` из ответа сознательно игнорируется: при `failed:3` VK
        // говорит «истории больше нет», и единственный корректный курсор —
        // тот, что выдаст `getLongPollServer` вместе с новым ключом.
        LpFailed::InfoLost { .. } => LpRecovery::RenewAll,
    }
}

/// `u64` из числа ИЛИ из строки.
///
/// LongPoll отдаёт `ts` то числом, то строкой в зависимости от версии; строгий
/// `as_u64` на второй форме молча превратил бы штатный ответ в `Malformed`.
fn as_u64_lenient(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

// ---------------------------------------------------------------------------
// Чистое ядро: разбор события.
// ---------------------------------------------------------------------------

/// Событие LongPoll в типизированном виде.
///
/// Разбор отделён от отображения в кадры ([`map_event`]) намеренно: разбор
/// зависит только от формата VK, а отображение — ещё и от того, что мы успели
/// узнать о сообщениях ([`MapCtx`]). Смешав их, нельзя было бы написать
/// табличный тест ни на одно, ни на другое.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LpEvent {
    /// 1 — флаги сообщения заменены целиком.
    FlagsReplace {
        /// Сообщение.
        message_id: i64,
        /// Новое значение флагов.
        flags: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 2 — биты флагов установлены.
    FlagsSet {
        /// Сообщение.
        message_id: i64,
        /// Установленные биты.
        mask: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 3 — биты флагов сброшены.
    FlagsReset {
        /// Сообщение.
        message_id: i64,
        /// Сброшенные биты.
        mask: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 4 — новое сообщение (ТРЕБУЕТ обогащения).
    NewMessage {
        /// Сообщение.
        message_id: i64,
        /// Флаги (бит 2 — исходящее).
        flags: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 5 — сообщение отредактировано (ТРЕБУЕТ обогащения).
    MessageEdited {
        /// Сообщение.
        message_id: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 18 — к сообщению добавился сниппет ссылки (ТРЕБУЕТ обогащения).
    SnippetAdded {
        /// Сообщение.
        message_id: i64,
        /// Беседа.
        peer_id: i64,
    },
    /// 6 — входящие прочитаны до `local_id`.
    ReadInbox {
        /// Беседа.
        peer_id: i64,
        /// Граница прочитанного.
        local_id: i64,
    },
    /// 7 — исходящие прочитаны до `local_id`.
    ReadOutbox {
        /// Беседа.
        peer_id: i64,
        /// Граница прочитанного.
        local_id: i64,
    },
    /// 8 — друг появился в сети.
    FriendOnline {
        /// Пользователь (уже БЕЗ минуса, который приходит от VK).
        user_id: i64,
        /// Код платформы.
        platform: i64,
        /// Время события.
        ts: i64,
    },
    /// 9 — друг вышел.
    FriendOffline {
        /// Пользователь (уже без минуса).
        user_id: i64,
        /// Флаги (0 — вышел сам, 1 — по тайм-ауту).
        flags: i64,
        /// Время события.
        ts: i64,
    },
    /// 10/11/12 — флаги беседы.
    DialogFlags {
        /// Исходный код события.
        code: i64,
        /// Беседа.
        peer_id: i64,
        /// Маска.
        mask: i64,
    },
    /// 13 — вся история беседы удалена до `up_to_id`.
    HistoryCleared {
        /// Беседа.
        peer_id: i64,
        /// Граница удаления.
        up_to_id: i64,
    },
    /// 51/52 — изменились данные беседы.
    ChatChanged {
        /// Беседа.
        peer_id: i64,
    },
    /// 61/62/63/64 — набор текста или запись голосового.
    Typing {
        /// Беседа.
        peer_id: i64,
        /// Кто именно (у 63/64 их несколько).
        from_ids: Vec<i64>,
        /// `typing` | `record_voice`.
        action: &'static str,
    },
    /// 70 — звонок.
    Call {
        /// Собеседник.
        user_id: i64,
    },
    /// 80 — общий счётчик непрочитанного.
    UnreadTotal {
        /// Значение счётчика.
        count: i64,
    },
    /// 114 — изменились настройки оповещений беседы.
    NotifySettings {
        /// Беседа.
        peer_id: i64,
        /// Заглушена ли.
        muted: bool,
    },
    /// Код, которого мы не знаем.
    ///
    /// Существует ради forward-compat: VK добавляет коды без предупреждения, и
    /// паника (или отказ разбирать всю пачку) означала бы, что новая фича VK
    /// ломает realtime целиком. Ровно то же правило, что у битого WS-кадра во
    /// фронте: warn и игнор.
    Unknown(i64),
}

/// Разбирает одно событие LongPoll.
///
/// **Событие — это МАССИВ**, а не объект: `[4, 7010, 17, 2000000001, …]`.
/// Первый элемент — код, остальные позиционны и зависят от кода.
///
/// `None` означает «это не событие»: пустой массив, код не числом, не хватает
/// обязательных полей. Такой вход просто пропускается — уронить пачку из-за
/// одного испорченного элемента было бы худшим из решений.
pub fn parse_event(raw: &[Value]) -> Option<LpEvent> {
    let code = raw.first().and_then(Value::as_i64)?;
    let ev = match code {
        1 => LpEvent::FlagsReplace {
            message_id: num(raw, 1)?,
            flags: num(raw, 2)?,
            peer_id: num(raw, 3)?,
        },
        2 => LpEvent::FlagsSet {
            message_id: num(raw, 1)?,
            mask: num(raw, 2)?,
            peer_id: num(raw, 3)?,
        },
        3 => LpEvent::FlagsReset {
            message_id: num(raw, 1)?,
            mask: num(raw, 2)?,
            peer_id: num(raw, 3)?,
        },
        4 => LpEvent::NewMessage {
            message_id: num(raw, 1)?,
            flags: num(raw, 2).unwrap_or(0),
            peer_id: num(raw, 3)?,
        },
        5 => LpEvent::MessageEdited {
            message_id: num(raw, 1)?,
            // У правки `peer_id` стоит на той же позиции, что у события 4, но
            // в части версий протокола его нет вовсе — беседу тогда возьмём из
            // самого сообщения при обогащении.
            peer_id: num(raw, 3).unwrap_or(0),
        },
        6 => LpEvent::ReadInbox {
            peer_id: num(raw, 1)?,
            local_id: num(raw, 2)?,
        },
        7 => LpEvent::ReadOutbox {
            peer_id: num(raw, 1)?,
            local_id: num(raw, 2)?,
        },
        // Идентификатор друга приезжает СО ЗНАКОМ МИНУС (`-123456`) — это
        // способ VK отличить событие присутствия от события беседы в общем
        // потоке. Наружу отдаём нормальный положительный id.
        8 => LpEvent::FriendOnline {
            user_id: num(raw, 1)?.abs(),
            platform: num(raw, 2).unwrap_or(0),
            ts: num(raw, 3).unwrap_or(0),
        },
        9 => LpEvent::FriendOffline {
            user_id: num(raw, 1)?.abs(),
            flags: num(raw, 2).unwrap_or(0),
            ts: num(raw, 3).unwrap_or(0),
        },
        10..=12 => LpEvent::DialogFlags {
            code,
            peer_id: num(raw, 1)?,
            mask: num(raw, 2).unwrap_or(0),
        },
        13 => LpEvent::HistoryCleared {
            peer_id: num(raw, 1)?,
            up_to_id: num(raw, 2).unwrap_or(0),
        },
        18 => LpEvent::SnippetAdded {
            message_id: num(raw, 1)?,
            peer_id: num(raw, 3).unwrap_or(0),
        },
        // 51 адресуется НОМЕРОМ БЕСЕДЫ, а не `peer_id`: у этого события своя,
        // историческая нумерация, и без сдвига кадр ушёл бы на несуществующего
        // пользователя с тем же числом.
        51 => LpEvent::ChatChanged {
            peer_id: CHAT_PEER_BASE + num(raw, 1)?,
        },
        52 => LpEvent::ChatChanged {
            peer_id: num(raw, 2)?,
        },
        // 61 — набор в личной беседе: собеседник и есть беседа.
        61 => {
            let user = num(raw, 1)?;
            LpEvent::Typing {
                peer_id: user,
                from_ids: vec![user],
                action: ACTION_TYPING,
            }
        }
        // 62 — набор в беседе: во втором поле её НОМЕР, не `peer_id`.
        62 => LpEvent::Typing {
            peer_id: CHAT_PEER_BASE + num(raw, 2)?,
            from_ids: vec![num(raw, 1)?],
            action: ACTION_TYPING,
        },
        63 => parse_typing_multi(raw, ACTION_TYPING)?,
        64 => parse_typing_multi(raw, ACTION_RECORD_VOICE)?,
        70 => LpEvent::Call {
            user_id: num(raw, 1)?,
        },
        80 => LpEvent::UnreadTotal {
            count: num(raw, 1)?,
        },
        114 => parse_notify_settings(raw)?,
        other => {
            tracing::warn!(
                target: "forge_vk",
                code = other,
                "forge-vk: unknown LongPoll event code, ignored"
            );
            LpEvent::Unknown(other)
        }
    };
    Some(ev)
}

/// События 63/64: массив идентификаторов + беседа.
///
/// Позиции `from_ids` и `peer_id` в разных версиях документации VK описаны
/// по-разному (`[63, [ids], peer_id, …]` и `[63, peer_id, [ids], …]`).
/// Разбираем по ТИПУ, а не по позиции: иначе на одной из версий мы бы молча
/// показывали «печатает» от пользователя с номером беседы.
fn parse_typing_multi(raw: &[Value], action: &'static str) -> Option<LpEvent> {
    let mut from_ids: Option<Vec<i64>> = None;
    let mut peer_id: Option<i64> = None;
    for slot in raw.iter().take(3).skip(1) {
        match slot {
            Value::Array(items) if from_ids.is_none() => {
                from_ids = Some(items.iter().filter_map(Value::as_i64).collect());
            }
            v if peer_id.is_none() => {
                if let Some(n) = v.as_i64() {
                    peer_id = Some(n);
                }
            }
            _ => {}
        }
    }
    Some(LpEvent::Typing {
        peer_id: peer_id?,
        from_ids: from_ids?,
        action,
    })
}

/// Событие 114: объект настроек оповещений.
///
/// `disabled_until`: `0` — не заглушена, `-1` — заглушена навсегда, иначе
/// unix-время окончания. Сравнивать с текущим временем здесь не нужно: VK
/// присылает событие в момент ИЗМЕНЕНИЯ, и «заглушена до вчера» в нём
/// появиться не может.
fn parse_notify_settings(raw: &[Value]) -> Option<LpEvent> {
    let obj = raw.get(1)?;
    let peer_id = obj.get("peer_id").and_then(Value::as_i64)?;
    let until = obj
        .get("disabled_until")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(LpEvent::NotifySettings {
        peer_id,
        muted: until != 0,
    })
}

/// Целое из позиции массива.
fn num(raw: &[Value], idx: usize) -> Option<i64> {
    raw.get(idx).and_then(Value::as_i64)
}

/// Нужен ли пачке дозапрос за полными сообщениями.
///
/// Только события 4/5/18 несут урезанное сообщение. Всё остальное (typing,
/// online, отметки прочтения, счётчики) отображается в кадры без единого
/// обращения к сети — именно поэтому «человек печатает» не тратит бюджет.
pub fn needs_enrichment(events: &[LpEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            LpEvent::NewMessage { .. } | LpEvent::MessageEdited { .. } | LpEvent::SnippetAdded { .. }
        )
    })
}

/// Идентификаторы сообщений, которые нужно добрать.
pub fn enrichment_ids(events: &[LpEvent]) -> Vec<i64> {
    let mut out = Vec::new();
    for e in events {
        let id = match e {
            LpEvent::NewMessage { message_id, .. }
            | LpEvent::MessageEdited { message_id, .. }
            | LpEvent::SnippetAdded { message_id, .. } => *message_id,
            _ => continue,
        };
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Чистое ядро: отображение события в кадры.
// ---------------------------------------------------------------------------

/// Что мы знаем о пачке на момент отображения.
///
/// Пустой контекст — законное состояние: пачка без событий 4/5/18 не требует
/// ни одного запроса, и заводить ради неё «пустое обогащение» означало бы
/// платить за typing.
#[derive(Debug, Clone, Default)]
pub struct MapCtx {
    /// `message_id` → полное сообщение.
    pub messages: HashMap<i64, MessageDto>,
    /// `peer_id` → заглушена ли беседа.
    pub muted: HashMap<i64, bool>,
}

impl MapCtx {
    /// Контекст без единого запроса.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Контекст из ответа `messages.getLongPollHistory`.
    ///
    /// `names` — источник отображаемых имён ([`crate::peers::PeerCache`] в
    /// бою, [`crate::dto::NoNames`] в тестах). Наполнять сам кэш обязан
    /// вызывающий: чистая функция не должна иметь побочных эффектов, иначе её
    /// нельзя было бы прогнать таблицей.
    pub fn from_history<N: PeerNames + ?Sized>(
        hist: &LongPollHistoryRaw,
        names: &N,
        now: i64,
    ) -> Self {
        let mut messages = HashMap::new();
        for raw in &hist.messages.items {
            messages.insert(raw.id, crate::dto::message_from_raw(raw, names));
        }
        let mut muted = HashMap::new();
        for conv in &hist.conversations {
            let is_muted = conv
                .push_settings
                .as_ref()
                .is_some_and(|p| p.disabled_forever || p.disabled_until > now);
            muted.insert(conv.peer.id, is_muted);
        }
        Self { messages, muted }
    }

    /// Добавляет сообщения, добранные `messages.getById` (путь деградации).
    pub fn absorb_messages<N: PeerNames + ?Sized>(
        &mut self,
        items: &[crate::api::schema::MessageRaw],
        names: &N,
    ) {
        for raw in items {
            self.messages
                .insert(raw.id, crate::dto::message_from_raw(raw, names));
        }
    }

    /// Полное сообщение, если оно приехало с обогащением.
    pub fn message(&self, id: i64) -> Option<&MessageDto> {
        self.messages.get(&id)
    }

    /// Заглушена ли беседа.
    ///
    /// «Не знаем» трактуется как «звенит»: решение о тосте принимает фронт по
    /// полю кадра, и молчаливое `true` спрятало бы сообщение, о котором нас
    /// просили сообщить. Обратная ошибка (лишний тост) исправляется одним
    /// щелчком, пропущенное сообщение — ничем.
    pub fn is_muted(&self, peer_id: i64) -> bool {
        self.muted.get(&peer_id).copied().unwrap_or(false)
    }
}

/// Отображает событие в кадры WS.
///
/// Возвращается ВЕКТОР, а не `Option`, ровно из-за событий 63/64: одно событие
/// «в беседе печатают трое» обязано стать тремя кадрами, иначе фронт увидит
/// только первого.
pub fn map_event(ev: &LpEvent, ctx: &MapCtx) -> Vec<VkServerMsg> {
    match ev {
        // Замена флагов целиком: какие именно биты изменились, из события не
        // видно, а перечитывать сообщение ради этого — запрос на событие.
        LpEvent::FlagsReplace { .. } => Vec::new(),

        LpEvent::FlagsSet {
            message_id,
            mask,
            peer_id,
        } => {
            if mask & FLAG_DELETED != 0 {
                vec![VkServerMsg::MessageDeleted {
                    dialog_id: dialog_id_string(*peer_id),
                    message_ids: vec![mid(*message_id)],
                }]
            } else {
                edited_if_known(ctx, *message_id)
            }
        }

        LpEvent::FlagsReset {
            message_id,
            mask,
            peer_id,
        } => {
            if mask & FLAG_UNREAD != 0 {
                vec![VkServerMsg::Read {
                    dialog_id: dialog_id_string(*peer_id),
                    max_id: mid(*message_id),
                    direction: DIRECTION_IN.to_string(),
                }]
            } else {
                edited_if_known(ctx, *message_id)
            }
        }

        LpEvent::NewMessage {
            message_id,
            peer_id,
            ..
        } => match ctx.message(*message_id) {
            Some(message) => vec![VkServerMsg::NewMessage {
                message: message.clone(),
                muted: ctx.is_muted(*peer_id),
                // У VK архива нет; поле сохранено ради идентичности
                // фронтового обработчика — см. шапку ws/protocol.rs.
                archived: false,
            }],
            // Обогащение отказало и добрать сообщение не вышло: честнее
            // попросить клиента перечитать состояние, чем нарисовать карточку
            // из урезанных полей, которая потом «мигнёт» при reconcile.
            None => vec![VkServerMsg::Resync],
        },

        LpEvent::MessageEdited { message_id, .. } | LpEvent::SnippetAdded { message_id, .. } => {
            match ctx.message(*message_id) {
                Some(message) => vec![VkServerMsg::MessageEdited {
                    message: message.clone(),
                }],
                None => vec![VkServerMsg::Resync],
            }
        }

        LpEvent::ReadInbox { peer_id, local_id } => vec![VkServerMsg::Read {
            dialog_id: dialog_id_string(*peer_id),
            max_id: mid(*local_id),
            direction: DIRECTION_IN.to_string(),
        }],

        LpEvent::ReadOutbox { peer_id, local_id } => vec![VkServerMsg::Read {
            dialog_id: dialog_id_string(*peer_id),
            max_id: mid(*local_id),
            direction: DIRECTION_OUT.to_string(),
        }],

        LpEvent::FriendOnline { user_id, .. } => vec![VkServerMsg::Online {
            user_id: user_id.to_string(),
            online: true,
            // «Был в сети» у только что вошедшего — это «сейчас», то есть
            // ничего не сообщающее значение. `null` честнее.
            was_online: None,
        }],

        LpEvent::FriendOffline { user_id, ts, .. } => vec![VkServerMsg::Online {
            user_id: user_id.to_string(),
            online: false,
            was_online: (*ts > 0).then_some(*ts),
        }],

        // Флаги беседы (важность, ответ, приглашение) UI не рисует: строка
        // списка перечитывается кадром dialog_changed, когда это правда нужно.
        LpEvent::DialogFlags { code, peer_id, mask } => {
            tracing::debug!(
                target: "forge_vk",
                code, peer_id, mask,
                "forge-vk: dialog flags event, no frame"
            );
            Vec::new()
        }

        LpEvent::HistoryCleared { peer_id, up_to_id } => vec![VkServerMsg::HistoryCleared {
            dialog_id: dialog_id_string(*peer_id),
            up_to_id: mid(*up_to_id),
        }],

        LpEvent::ChatChanged { peer_id } => vec![VkServerMsg::DialogChanged {
            dialog_id: dialog_id_string(*peer_id),
        }],

        LpEvent::Typing {
            peer_id,
            from_ids,
            action,
        } => from_ids
            .iter()
            .map(|from| VkServerMsg::Typing {
                dialog_id: dialog_id_string(*peer_id),
                user_id: from.to_string(),
                action: (*action).to_string(),
            })
            .collect(),

        // Звонок в терминальном UI нечем показать, а тост о нём звал бы туда,
        // куда мы не умеем перейти.
        LpEvent::Call { user_id } => {
            tracing::debug!(target: "forge_vk", user_id, "forge-vk: call event, no frame");
            Vec::new()
        }

        LpEvent::UnreadTotal { count } => vec![VkServerMsg::UnreadTotal {
            count: mid(*count),
        }],

        LpEvent::NotifySettings { peer_id, muted } => vec![VkServerMsg::NotifySettings {
            dialog_id: dialog_id_string(*peer_id),
            muted: *muted,
        }],

        LpEvent::Unknown(_) => Vec::new(),
    }
}

/// Кадр правки, если полное сообщение у нас есть.
///
/// Пусто, если нет: события 2 и 3 обогащения НЕ вызывают (иначе смена флага
/// «важное» стоила бы запроса), а `resync` на каждое изменение флагов
/// превратил бы прокрутку чужой ленты в шторм перезагрузок.
fn edited_if_known(ctx: &MapCtx, message_id: i64) -> Vec<VkServerMsg> {
    match ctx.message(message_id) {
        Some(message) => vec![VkServerMsg::MessageEdited {
            message: message.clone(),
        }],
        None => Vec::new(),
    }
}

/// Идентификаторы, которые надо выбросить из медиа-кэша.
///
/// Возвращает пары `(dialog_id, message_id)`: правка сообщения могла заменить
/// вложение, а удаление делает закэшированный файл следом того, чего уже нет.
/// Новые сообщения не инвалидируют ничего — им ещё нечего было закэшировать.
pub fn invalidations(frames: &[VkServerMsg]) -> Vec<(String, i32)> {
    let mut out: Vec<(String, i32)> = Vec::new();
    let mut push = |dialog: String, id: i32| {
        let pair = (dialog, id);
        if !out.contains(&pair) {
            out.push(pair);
        }
    };
    for frame in frames {
        match frame {
            VkServerMsg::MessageEdited { message } => {
                push(message.dialog_id.clone(), message.id);
            }
            VkServerMsg::MessageDeleted {
                dialog_id,
                message_ids,
            } => {
                for id in message_ids {
                    push(dialog_id.clone(), *id);
                }
            }
            _ => {}
        }
    }
    out
}

/// Сужает идентификатор до `i32`, которым оперирует протокол.
///
/// Насыщение вместо `unwrap`: у VK идентификаторы сообщений 32-битные, но
/// приезжают числом без объявленной ширины, и испорченное значение лучше
/// паники в фоновом воркере.
fn mid(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

// ---------------------------------------------------------------------------
// Курсор: получение и персист.
// ---------------------------------------------------------------------------

/// Курсор на диске.
///
/// Обёртка над [`LpCursor`] со временем записи. Время нужно затем, что `ts`
/// LongPoll — это ВНУТРЕННИЙ счётчик событий VK, а не unix-время: по нему
/// нельзя понять, протух курсор или нет. Без отметки протухший курсор давал бы
/// код 10 на каждом цикле, и поток не поднялся бы никогда.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCursor {
    /// Сам курсор.
    cursor: LpCursor,
    /// Unix-время записи.
    saved_at: i64,
}

/// Путь файла курсора.
pub fn cursor_path(cfg: &crate::config::VkConfig) -> PathBuf {
    cfg.cache_dir.join(CURSOR_FILE)
}

/// Читает сохранённый курсор.
///
/// `None` — это норма: файла нет (первый запуск), он битый или курсор старше
/// [`CURSOR_MAX_AGE_SECS`]. Во всех трёх случаях поток начнётся «с сейчас», и
/// это ЕДИНСТВЕННОЕ честное поведение: холодного catch-up VK не даёт.
pub fn load_cursor(path: &Path, now: i64) -> Option<LpCursor> {
    let bytes = std::fs::read(path).ok()?;
    let saved: PersistedCursor = serde_json::from_slice(&bytes)
        .map_err(|e| {
            tracing::warn!(
                target: "forge_vk",
                error = %e,
                "forge-vk: longpoll cursor file is unreadable, starting from now"
            );
        })
        .ok()?;
    let age = now.saturating_sub(saved.saved_at);
    if !(0..=CURSOR_MAX_AGE_SECS).contains(&age) {
        tracing::info!(
            target: "forge_vk",
            age_secs = age,
            "forge-vk: stored LongPoll cursor is stale, starting from now"
        );
        return None;
    }
    if saved.cursor.server.is_empty() || saved.cursor.key.is_empty() || saved.cursor.ts == 0 {
        return None;
    }
    Some(saved.cursor)
}

/// Пишет курсор на диск.
///
/// Права `0600` — ради единообразия с `vk.toml`: секрета в файле нет, но
/// `key` LongPoll производен от токена, и разрешать его чтение соседям по
/// машине незачем.
pub fn save_cursor(path: &Path, cursor: &LpCursor, now: i64) {
    let payload = PersistedCursor {
        cursor: cursor.clone(),
        saved_at: now,
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = crate::config::write_secret_file(path, &bytes) {
        tracing::warn!(
            target: "forge_vk",
            error = %e,
            "forge-vk: could not persist the LongPoll cursor"
        );
    }
}

/// Запрашивает у VK свежий адрес, ключ и позицию.
///
/// `need_pts=1` не опция: без `pts` нельзя вызвать `getLongPollHistory`, и
/// обогащение пачки одним запросом деградировало бы до запроса на сообщение
/// (см. шапку модуля).
///
/// # Errors
///
/// Любая ошибка транспорта или VK; код 5/14/15 вызывающий превращает в
/// соответствующую ветку выхода воркера.
pub async fn obtain_cursor(client: &VkClient) -> Result<LpCursor, VkError> {
    let raw = client.get_longpoll_server().await?;
    if raw.server.is_empty() || raw.key.is_empty() {
        return Err(VkError::Decode {
            reason: "messages.getLongPollServer returned an empty server/key".to_string(),
        });
    }
    Ok(LpCursor {
        server: raw.server,
        key: raw.key,
        ts: raw.ts,
        // Ноль означает «VK не прислал pts» — а не «pts равен нулю»: с
        // `need_pts=1` значение всегда положительное.
        pts: (raw.pts > 0).then_some(raw.pts),
    })
}

// ---------------------------------------------------------------------------
// Supervisor.
// ---------------------------------------------------------------------------

/// Причина, по которой цикл воркера завершился.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerExit {
    /// Пришёл сигнал остановки.
    Shutdown,
    /// VK сказал «токен невалиден» (код 5).
    AuthInvalid(String),
    /// VK сказал «нет права messages» (код 15).
    ScopeLost(String),
    /// VK потребовал captcha (код 14).
    Captcha {
        /// Идентификатор captcha.
        sid: String,
        /// Адрес картинки.
        img: String,
    },
    /// Всё остальное — повторяем с backoff'ом.
    Failed(String),
}

/// Итог одного захода в цикл.
struct WorkerOutcome {
    /// Почему вышли.
    exit: WorkerExit,
    /// Сколько успешных циклов опроса успело пройти.
    ///
    /// Нужно супервизору: лестница backoff'а обязана сбрасываться после
    /// успеха, иначе одна долгая сессия с единственным обрывом в конце уводила
    /// бы следующую попытку на минуту.
    cycles: u64,
}

/// Запускает supervisor как фонового воркера.
///
/// Вызывается ВСЕГДА, даже без токена: воркер сам ждёт авторизации, поэтому
/// вставка токена в работающем процессе поднимает realtime БЕЗ рестарта
/// devforge.
pub async fn spawn(state: &Arc<VkState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { supervisor(st).await });
    state.register_worker(handle).await;
    tracing::info!(target: "forge_vk", "forge-vk: longpoll supervisor spawned");
}

/// Вечный цикл: ждать авторизации → получить курсор → крутить опрос.
pub async fn supervisor(state: Arc<VkState>) {
    let mut step = 0usize;
    let last_step = state.lp.last_step();

    loop {
        if !wait_authorized(&state).await {
            tracing::info!(target: "forge_vk", "forge-vk: longpoll supervisor stopped");
            return;
        }

        let client = match state.api_client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "forge_vk", error = %e, "forge-vk: no API client for longpoll");
                if !backoff_sleep(&state, step).await {
                    return;
                }
                step = (step + 1).min(last_step);
                continue;
            }
        };

        let cursor = match start_cursor(&state, &client).await {
            Ok(c) => c,
            Err(e) => {
                if let Some(exit) = exit_for(&e) {
                    if handle_terminal_exit(&state, exit).await {
                        step = 0;
                        continue;
                    }
                    return;
                }
                tracing::warn!(target: "forge_vk", error = %e, "forge-vk: could not obtain the LongPoll cursor");
                if !backoff_sleep(&state, step).await {
                    return;
                }
                step = (step + 1).min(last_step);
                continue;
            }
        };

        let outcome = run_worker(&state, &client, cursor).await;
        if outcome.cycles > 0 {
            step = 0;
        }
        match outcome.exit {
            WorkerExit::Shutdown => {
                tracing::info!(target: "forge_vk", "forge-vk: longpoll supervisor stopped");
                return;
            }
            exit @ (WorkerExit::AuthInvalid(_)
            | WorkerExit::ScopeLost(_)
            | WorkerExit::Captcha { .. }) => {
                if !handle_terminal_exit(&state, exit).await {
                    return;
                }
                // РЕКОННЕКТА НЕТ: состояние логина только что перестало быть
                // `Authorized`, и следующий виток заблокируется в
                // wait_authorized до нового токена (или до решённой captcha).
                step = 0;
            }
            WorkerExit::Failed(reason) => {
                tracing::warn!(target: "forge_vk", reason = %reason, step, "forge-vk: longpoll worker failed");
                if !backoff_sleep(&state, step).await {
                    return;
                }
                step = (step + 1).min(last_step);
            }
        }
    }
}

/// Применяет терминальную ветку выхода к автомату логина.
///
/// `false` ⇒ пришёл shutdown и супервизору пора выходить.
async fn handle_terminal_exit(state: &Arc<VkState>, exit: WorkerExit) -> bool {
    match exit {
        WorkerExit::AuthInvalid(reason) => {
            crate::login::on_auth_invalidated(state, reason).await;
        }
        WorkerExit::ScopeLost(reason) => {
            crate::login::on_scope_lost(state, reason).await;
        }
        WorkerExit::Captcha { sid, img } => {
            crate::login::on_captcha(state, sid, img).await;
        }
        WorkerExit::Shutdown => return false,
        WorkerExit::Failed(_) => {}
    }
    !state.shutdown.is_cancelled()
}

/// Ждёт, пока плагин станет авторизованным.
///
/// `false` ⇒ дождались не авторизации, а остановки.
///
/// **`notified().enable()` вызывается ДО проверки состояния.** Без этого
/// классическая гонка «токен вставили между проверкой и подпиской» подвесила
/// бы воркера до следующего тика: `Notify::notify_waiters` будит только тех,
/// кто уже ждёт, и уведомление, пришедшее в этом окне, пропало бы бесследно.
async fn wait_authorized(state: &Arc<VkState>) -> bool {
    loop {
        let notified = state.authorized_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        if state.shutdown.is_cancelled() {
            return false;
        }
        if state.is_authorized().await {
            return true;
        }

        tokio::select! {
            biased;
            () = state.shutdown.cancelled() => return false,
            () = &mut notified => {}
            () = tokio::time::sleep(state.lp.auth_poll) => {}
        }
    }
}

/// Пауза перед следующей попыткой. `false` ⇒ пришёл shutdown.
async fn backoff_sleep(state: &Arc<VkState>, step: usize) -> bool {
    let delay = state.lp.backoff_at(step);
    tokio::select! {
        biased;
        () = state.shutdown.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Курсор для нового захода: сохранённый (если жив) или свежий от VK.
async fn start_cursor(state: &Arc<VkState>, client: &VkClient) -> Result<LpCursor, VkError> {
    if let Some(saved) = state.lp_cursor().await {
        return Ok(saved);
    }
    let path = cursor_path(&state.config);
    if let Some(stored) = load_cursor(&path, now_unix()) {
        tracing::info!(target: "forge_vk", "forge-vk: resuming LongPoll from the stored cursor");
        state.set_lp_cursor(Some(stored.clone())).await;
        return Ok(stored);
    }
    let fresh = obtain_cursor(client).await?;
    state.set_lp_cursor(Some(fresh.clone())).await;
    Ok(fresh)
}

/// Цикл опроса LongPoll-сервера.
async fn run_worker(state: &Arc<VkState>, client: &VkClient, mut cursor: LpCursor) -> WorkerOutcome {
    let wait = state.lp.wait_secs;
    let persist_every = state.lp.persist_every.max(1);
    let mut cycles: u64 = 0;
    let mut since_persist: u32 = 0;

    let exit = loop {
        let ts_before = cursor.ts;
        let poll = tokio::select! {
            biased;
            () = state.shutdown.cancelled() => break WorkerExit::Shutdown,
            r = client.longpoll_check(&cursor.server, &cursor.key, cursor.ts, wait, LP_MODE) => r,
        };

        let body = match poll {
            Ok(v) => v,
            Err(e) => break WorkerExit::Failed(e.to_string()),
        };

        match parse_poll(&body) {
            LpPoll::Malformed(reason) => break WorkerExit::Failed(format!("malformed poll: {reason}")),

            LpPoll::Failed(failed) => {
                tracing::info!(target: "forge_vk", failed = ?failed, "forge-vk: LongPoll asked for recovery");
                match recovery_for(&failed) {
                    LpRecovery::UseTs(new_ts) => {
                        cursor.ts = new_ts;
                    }
                    LpRecovery::RenewKeyKeepTs => {
                        let fresh = match obtain_cursor(client).await {
                            Ok(c) => c,
                            Err(e) => break exit_for(&e).unwrap_or(WorkerExit::Failed(e.to_string())),
                        };
                        // `ts` НЕ берётся из ответа: пока ключ протухал, на
                        // сервере копились события, а свежий `ts` указывает на
                        // «сейчас». Взяв его, мы потеряли бы всё накопленное.
                        cursor.server = fresh.server;
                        cursor.key = fresh.key;
                        if cursor.pts.is_none() {
                            cursor.pts = fresh.pts;
                        }
                    }
                    LpRecovery::RenewAll => {
                        let fresh = match obtain_cursor(client).await {
                            Ok(c) => c,
                            Err(e) => break exit_for(&e).unwrap_or(WorkerExit::Failed(e.to_string())),
                        };
                        cursor = fresh;
                        // История на сервере оборвана — клиенту остаётся
                        // перечитать состояние по REST.
                        state.broadcast_event(VkServerMsg::Resync);
                    }
                }
                state.set_lp_cursor(Some(cursor.clone())).await;
                continue;
            }

            LpPoll::Updates { ts, pts, updates } => {
                cursor.ts = ts;
                if let Some(p) = pts {
                    cursor.pts = Some(p);
                }

                let events: Vec<LpEvent> = updates
                    .iter()
                    .filter_map(|raw| parse_event(raw))
                    .collect();

                let ctx = if needs_enrichment(&events) {
                    match enrich(state, client, &events, ts_before, cursor.pts).await {
                        Ok((ctx, new_pts)) => {
                            if let Some(p) = new_pts {
                                cursor.pts = Some(p);
                            }
                            ctx
                        }
                        Err(e) => {
                            if let Some(exit) = exit_for(&e) {
                                break exit;
                            }
                            // Обогащение отказало окончательно: кадры всё
                            // равно уйдут, но события 4/5/18 закроются
                            // `resync` (см. map_event).
                            tracing::warn!(
                                target: "forge_vk",
                                error = %e,
                                "forge-vk: enrichment failed, falling back to resync frames"
                            );
                            MapCtx::empty()
                        }
                    }
                } else {
                    MapCtx::empty()
                };

                let frames: Vec<VkServerMsg> =
                    events.iter().flat_map(|e| map_event(e, &ctx)).collect();
                let dropped = invalidations(&frames);
                if !dropped.is_empty() {
                    // Ответ медиа-роута помечен `Cache-Control: immutable`,
                    // поэтому без инвалидации отредактированное вложение
                    // осталось бы старым НАВСЕГДА — и на диске, и во вкладке.
                    let mut removed = 0;
                    for (dialog_id, message_id) in &dropped {
                        removed +=
                            crate::media_cache::invalidate(&state.config, dialog_id, *message_id)
                                .await;
                        if let Some(peer) = crate::dto::parse_dialog_id(dialog_id) {
                            // И короткоживущий кэш сообщения: в нём лежат
                            // адреса вложений той версии, которой уже нет.
                            state.forget_message(peer, *message_id).await;
                        }
                    }
                    tracing::debug!(
                        target: "forge_vk",
                        count = dropped.len(),
                        removed,
                        "forge-vk: media cache entries invalidated by LongPoll"
                    );
                }
                for frame in frames {
                    state.broadcast_event(frame);
                }

                state.set_lp_cursor(Some(cursor.clone())).await;
                cycles += 1;
                since_persist += 1;
                if since_persist >= persist_every {
                    since_persist = 0;
                    save_cursor(&cursor_path(&state.config), &cursor, now_unix());
                }
            }
        }
    };

    // Курсор персистится ДО выхода на ЛЮБОЙ ветке: после рестарта поток должен
    // продолжиться с той же позиции, а не «с сейчас».
    state.set_lp_cursor(Some(cursor.clone())).await;
    save_cursor(&cursor_path(&state.config), &cursor, now_unix());
    WorkerOutcome { exit, cycles }
}

/// Обогащает пачку: один запрос `messages.getLongPollHistory`.
///
/// Возвращает контекст и новый `pts`. При отказе метода пробует добрать не
/// более [`MAX_FALLBACK_GETBYID`] сообщений через `messages.getById` — это
/// ЕДИНСТВЕННОЕ место, где запрос на сообщение допустим, и цена ограничена
/// сверху. Всё, что не добрали, закроется кадром `resync` в [`map_event`].
async fn enrich(
    state: &Arc<VkState>,
    client: &VkClient,
    events: &[LpEvent],
    ts: u64,
    pts: Option<u64>,
) -> Result<(MapCtx, Option<u64>), VkError> {
    let now = now_unix();
    match client.get_longpoll_history(ts, pts).await {
        Ok(hist) => {
            // Имена, приехавшие бесплатно вместе с историей, — то самое, ради
            // чего вариант с `getById` был бы вдвойне дорог.
            state.peers.absorb_extended(&hist.profiles, &hist.groups);
            let ctx = MapCtx::from_history(&hist, &state.peers, now);
            let new_pts = (hist.new_pts > 0).then_some(hist.new_pts);
            Ok((ctx, new_pts))
        }
        Err(e) => {
            // Терминальные коды наверх без попыток: добирать сообщения мёртвым
            // токеном бессмысленно.
            if exit_for(&e).is_some() {
                return Err(e);
            }
            if e.code() == Some(CODE_INTERNAL_SERVER) {
                tracing::warn!(
                    target: "forge_vk",
                    "forge-vk: getLongPollHistory says the cursor is too old (code 10), degrading"
                );
            }
            let ids: Vec<i64> = enrichment_ids(events)
                .into_iter()
                .take(MAX_FALLBACK_GETBYID)
                .collect();
            if ids.is_empty() {
                return Err(e);
            }
            let page = client.get_by_id(&ids).await?;
            state.peers.absorb_extended(&page.profiles, &page.groups);
            let mut ctx = MapCtx::empty();
            ctx.absorb_messages(&page.items, &state.peers);
            Ok((ctx, None))
        }
    }
}

/// Терминальная ветка выхода для ошибки VK, если она терминальная.
fn exit_for(err: &VkError) -> Option<WorkerExit> {
    match err.code() {
        Some(CODE_AUTH_FAILED) => Some(WorkerExit::AuthInvalid(err.to_string())),
        Some(CODE_ACCESS_DENIED) => Some(WorkerExit::ScopeLost(err.to_string())),
        Some(CODE_CAPTCHA) => err.captcha().map(|(sid, img)| WorkerExit::Captcha {
            sid: sid.to_string(),
            img: img.to_string(),
        }),
        _ => None,
    }
}

/// Текущее unix-время.
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::NoNames;
    use serde_json::json;

    fn arr(v: Value) -> Vec<Value> {
        v.as_array().cloned().unwrap_or_default()
    }

    // -------------------------------------------------------------------
    // Константы.
    // -------------------------------------------------------------------

    /// Режим и версия — это контракт с VK, а не настройка: бит 32 (pts) без
    /// теста однажды «оптимизировали» бы обратно, и обогащение пачки одним
    /// запросом молча превратилось бы в запрос на сообщение.
    #[test]
    fn lp_mode_carries_every_required_bit() {
        assert_eq!(LP_MODE, 234, "2|8|32|64|128");
        assert_eq!(LP_MODE & 32, 32, "бит pts обязателен для getLongPollHistory");
        assert_eq!(LP_MODE & 2, 2, "текст сообщения");
        assert_eq!(LP_MODE & 8, 8, "расширенные события");
        assert_eq!(LP_VERSION, 3);
        assert_eq!(LP_WAIT_SECS, 25);
        assert_eq!(BACKOFF_SECS, [1, 2, 5, 10, 30, 60]);
        assert_eq!(AUTH_POLL, Duration::from_secs(2));
    }

    // -------------------------------------------------------------------
    // parse_poll.
    // -------------------------------------------------------------------

    #[test]
    fn parse_poll_reads_updates() {
        let body = json!({"ts": 42, "pts": 7, "updates": [[4, 1, 17, 1234], [6, 1234, 9]]});
        match parse_poll(&body) {
            LpPoll::Updates { ts, pts, updates } => {
                assert_eq!(ts, 42);
                assert_eq!(pts, Some(7));
                assert_eq!(updates.len(), 2);
                assert_eq!(updates[0][0], json!(4));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Пустой `updates` и его полное отсутствие — ШТАТНЫЙ выход по тайм-ауту
    /// ожидания, а не ошибка: именно так выглядит «за 25 секунд ничего не
    /// произошло».
    #[test]
    fn parse_poll_accepts_an_empty_batch() {
        for body in [json!({"ts": 5, "updates": []}), json!({"ts": 5})] {
            match parse_poll(&body) {
                LpPoll::Updates { ts, updates, .. } => {
                    assert_eq!(ts, 5);
                    assert!(updates.is_empty());
                }
                other => panic!("{body} → {other:?}"),
            }
        }
    }

    /// `ts` строкой — вторая наблюдаемая форма ответа VK.
    #[test]
    fn parse_poll_survives_a_stringly_typed_ts() {
        match parse_poll(&json!({"ts": "1700", "updates": []})) {
            LpPoll::Updates { ts, .. } => assert_eq!(ts, 1700),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_poll_maps_every_failed_code() {
        assert_eq!(
            parse_poll(&json!({"failed": 1, "ts": 99})),
            LpPoll::Failed(LpFailed::StaleTs { new_ts: 99 })
        );
        assert_eq!(
            parse_poll(&json!({"failed": 2})),
            LpPoll::Failed(LpFailed::ExpiredKey)
        );
        assert_eq!(
            parse_poll(&json!({"failed": 3})),
            LpPoll::Failed(LpFailed::InfoLost { new_ts: None })
        );
        assert_eq!(
            parse_poll(&json!({"failed": 3, "ts": 5})),
            LpPoll::Failed(LpFailed::InfoLost { new_ts: Some(5) })
        );
    }

    /// `failed:1` БЕЗ `ts` — нарушение протокола: сервер объявил наш `ts`
    /// устаревшим, но не сказал, куда встать. Подстановка нуля означала бы
    /// «отдай всю историю с начала времён».
    #[test]
    fn parse_poll_rejects_failed_one_without_ts() {
        match parse_poll(&json!({"failed": 1})) {
            LpPoll::Malformed(reason) => assert!(reason.contains("failed:1"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_poll_reports_garbage() {
        for body in [
            json!({}),
            json!({"updates": []}),
            json!({"failed": 42}),
            json!({"ts": 1, "updates": "nope"}),
            json!("captive portal"),
        ] {
            assert!(
                matches!(parse_poll(&body), LpPoll::Malformed(_)),
                "{body} обязан быть Malformed"
            );
        }
    }

    // -------------------------------------------------------------------
    // recovery_for.
    // -------------------------------------------------------------------

    /// Три ветки протокола `failed` — дословно. Главная из них — вторая:
    /// `ts` из ответа НЕ берётся, иначе теряются события, накопленные, пока
    /// ключ протухал.
    #[test]
    fn recovery_follows_the_protocol() {
        assert_eq!(
            recovery_for(&LpFailed::StaleTs { new_ts: 77 }),
            LpRecovery::UseTs(77)
        );
        assert_eq!(recovery_for(&LpFailed::ExpiredKey), LpRecovery::RenewKeyKeepTs);
        assert_eq!(
            recovery_for(&LpFailed::InfoLost { new_ts: None }),
            LpRecovery::RenewAll
        );
        // `ts` в failed:3 присутствует, но на решение НЕ влияет: курсор
        // сбрасывается целиком.
        assert_eq!(
            recovery_for(&LpFailed::InfoLost { new_ts: Some(123) }),
            LpRecovery::RenewAll
        );
    }

    // -------------------------------------------------------------------
    // parse_event — таблица по ВСЕМ кодам.
    // -------------------------------------------------------------------

    #[test]
    fn parse_event_table_covers_every_documented_code() {
        let cases: Vec<(Value, LpEvent)> = vec![
            (
                json!([1, 7010, 17, 1234]),
                LpEvent::FlagsReplace {
                    message_id: 7010,
                    flags: 17,
                    peer_id: 1234,
                },
            ),
            (
                json!([2, 7010, 128, 1234]),
                LpEvent::FlagsSet {
                    message_id: 7010,
                    mask: 128,
                    peer_id: 1234,
                },
            ),
            (
                json!([3, 7010, 1, 1234]),
                LpEvent::FlagsReset {
                    message_id: 7010,
                    mask: 1,
                    peer_id: 1234,
                },
            ),
            (
                json!([4, 7011, 17, 2000000001, 1800000000, "привет", {}, {}]),
                LpEvent::NewMessage {
                    message_id: 7011,
                    flags: 17,
                    peer_id: 2000000001,
                },
            ),
            (
                json!([5, 7011, 0, 1234, 1800000000, "правка"]),
                LpEvent::MessageEdited {
                    message_id: 7011,
                    peer_id: 1234,
                },
            ),
            (
                json!([6, 1234, 7009]),
                LpEvent::ReadInbox {
                    peer_id: 1234,
                    local_id: 7009,
                },
            ),
            (
                json!([7, 1234, 7009]),
                LpEvent::ReadOutbox {
                    peer_id: 1234,
                    local_id: 7009,
                },
            ),
            (
                json!([8, -1234, 7, 1800000000]),
                LpEvent::FriendOnline {
                    user_id: 1234,
                    platform: 7,
                    ts: 1800000000,
                },
            ),
            (
                json!([9, -1234, 0, 1800000000]),
                LpEvent::FriendOffline {
                    user_id: 1234,
                    flags: 0,
                    ts: 1800000000,
                },
            ),
            (
                json!([10, 1234, 8]),
                LpEvent::DialogFlags {
                    code: 10,
                    peer_id: 1234,
                    mask: 8,
                },
            ),
            (
                json!([11, 1234, 8]),
                LpEvent::DialogFlags {
                    code: 11,
                    peer_id: 1234,
                    mask: 8,
                },
            ),
            (
                json!([12, 1234, 8]),
                LpEvent::DialogFlags {
                    code: 12,
                    peer_id: 1234,
                    mask: 8,
                },
            ),
            (
                json!([13, 2000000001, 7020]),
                LpEvent::HistoryCleared {
                    peer_id: 2000000001,
                    up_to_id: 7020,
                },
            ),
            (
                json!([18, 7011, 0, 1234]),
                LpEvent::SnippetAdded {
                    message_id: 7011,
                    peer_id: 1234,
                },
            ),
            (
                json!([51, 1, 1]),
                LpEvent::ChatChanged {
                    peer_id: CHAT_PEER_BASE + 1,
                },
            ),
            (
                json!([52, 1, 2000000001, 0]),
                LpEvent::ChatChanged {
                    peer_id: 2000000001,
                },
            ),
            (
                json!([61, 1234, 1]),
                LpEvent::Typing {
                    peer_id: 1234,
                    from_ids: vec![1234],
                    action: ACTION_TYPING,
                },
            ),
            (
                json!([62, 1234, 1]),
                LpEvent::Typing {
                    peer_id: CHAT_PEER_BASE + 1,
                    from_ids: vec![1234],
                    action: ACTION_TYPING,
                },
            ),
            (
                json!([63, [1234, 5678], 2000000001, 2, 1800000000]),
                LpEvent::Typing {
                    peer_id: 2000000001,
                    from_ids: vec![1234, 5678],
                    action: ACTION_TYPING,
                },
            ),
            (
                json!([64, [1234], 2000000001, 1, 1800000000]),
                LpEvent::Typing {
                    peer_id: 2000000001,
                    from_ids: vec![1234],
                    action: ACTION_RECORD_VOICE,
                },
            ),
            (json!([70, 1234, 99]), LpEvent::Call { user_id: 1234 }),
            (json!([80, 12, 0]), LpEvent::UnreadTotal { count: 12 }),
            (
                json!([114, {"peer_id": 2000000001, "sound": 0, "disabled_until": -1}]),
                LpEvent::NotifySettings {
                    peer_id: 2000000001,
                    muted: true,
                },
            ),
            (
                json!([114, {"peer_id": 1234, "sound": 1, "disabled_until": 0}]),
                LpEvent::NotifySettings {
                    peer_id: 1234,
                    muted: false,
                },
            ),
            (json!([999, 1, 2]), LpEvent::Unknown(999)),
        ];

        for (raw, expected) in cases {
            let got = parse_event(&arr(raw.clone()));
            assert_eq!(got.as_ref(), Some(&expected), "вход {raw}");
        }
    }

    /// Порядок полей у 63/64 в разных версиях документации VK описан
    /// по-разному. Разбор идёт по ТИПУ, поэтому ОБЕ формы дают один результат
    /// — иначе на одной из них «печатает» приписывалось бы пользователю с
    /// номером беседы.
    #[test]
    fn typing_multi_is_parsed_by_type_not_by_position() {
        let expected = LpEvent::Typing {
            peer_id: 2000000001,
            from_ids: vec![11, 22],
            action: ACTION_TYPING,
        };
        assert_eq!(
            parse_event(&arr(json!([63, [11, 22], 2000000001, 2, 1]))),
            Some(expected.clone())
        );
        assert_eq!(
            parse_event(&arr(json!([63, 2000000001, [11, 22], 2, 1]))),
            Some(expected)
        );
    }

    /// Мусорные входы: ни один не имеет права уронить разбор пачки.
    #[test]
    fn parse_event_rejects_garbage() {
        let bad = vec![
            json!([]),                    // пустой массив
            json!(["4", 1, 2, 3]),        // код строкой
            json!([4]),                   // не хватает полей
            json!([4, 7011]),             // нет peer_id
            json!([6, 1234]),             // нет local_id
            json!([13]),                  // нет peer_id
            json!([114]),                 // нет объекта настроек
            json!([114, {"sound": 0}]),   // объект без peer_id
            json!([63, 1, 2, 3]),         // нет массива from_ids
            json!([61]),                  // нет пользователя
            json!([80]),                  // нет счётчика
            json!([null, 1, 2]),          // код не число
        ];
        for raw in bad {
            assert_eq!(parse_event(&arr(raw.clone())), None, "вход {raw}");
        }
    }

    /// Неизвестный код не роняет пачку и не паникует — он становится
    /// `Unknown`, который [`map_event`] отображает в ноль кадров.
    #[test]
    fn unknown_codes_are_forward_compatible() {
        for code in [99, 115, 1000, i64::from(i32::MAX)] {
            let ev = parse_event(&arr(json!([code, 1, 2, 3]))).unwrap();
            assert_eq!(ev, LpEvent::Unknown(code));
            assert!(map_event(&ev, &MapCtx::empty()).is_empty());
        }
    }

    // -------------------------------------------------------------------
    // needs_enrichment.
    // -------------------------------------------------------------------

    /// Ноль запросов на typing/online/read — то, ради чего вообще существует
    /// эта функция при бюджете 3 rps.
    #[test]
    fn enrichment_is_required_only_by_message_events() {
        let cheap = vec![
            LpEvent::Typing {
                peer_id: 1,
                from_ids: vec![1],
                action: ACTION_TYPING,
            },
            LpEvent::ReadInbox {
                peer_id: 1,
                local_id: 2,
            },
            LpEvent::FriendOnline {
                user_id: 1,
                platform: 0,
                ts: 0,
            },
            LpEvent::UnreadTotal { count: 3 },
            LpEvent::Unknown(999),
        ];
        assert!(!needs_enrichment(&cheap));
        assert!(!needs_enrichment(&[]));

        for costly in [
            LpEvent::NewMessage {
                message_id: 1,
                flags: 0,
                peer_id: 2,
            },
            LpEvent::MessageEdited {
                message_id: 1,
                peer_id: 2,
            },
            LpEvent::SnippetAdded {
                message_id: 1,
                peer_id: 2,
            },
        ] {
            let mut batch = cheap.clone();
            batch.push(costly.clone());
            assert!(needs_enrichment(&batch), "{costly:?}");
        }
    }

    /// Идентификаторы для деградации собираются без дублей: пачка из десяти
    /// событий про одно сообщение обязана стоить один `getById`, а не десять.
    #[test]
    fn enrichment_ids_are_deduplicated_and_ordered() {
        let events = vec![
            LpEvent::NewMessage {
                message_id: 5,
                flags: 0,
                peer_id: 1,
            },
            LpEvent::MessageEdited {
                message_id: 5,
                peer_id: 1,
            },
            LpEvent::SnippetAdded {
                message_id: 7,
                peer_id: 1,
            },
            LpEvent::ReadInbox {
                peer_id: 1,
                local_id: 9,
            },
        ];
        assert_eq!(enrichment_ids(&events), vec![5, 7]);
    }

    // -------------------------------------------------------------------
    // map_event — событие + MapCtx → вектор кадров.
    // -------------------------------------------------------------------

    fn msg(id: i32, peer: i64) -> MessageDto {
        MessageDto {
            id,
            dialog_id: dialog_id_string(peer),
            text: "привет".into(),
            date: 1_800_000_000,
            ..MessageDto::default()
        }
    }

    fn ctx_with(id: i64, peer: i64, muted: bool) -> MapCtx {
        let mut ctx = MapCtx::empty();
        ctx.messages.insert(id, msg(id as i32, peer));
        ctx.muted.insert(peer, muted);
        ctx
    }

    #[test]
    fn map_event_table_matches_the_documented_frames() {
        let peer = 2_000_000_001_i64;
        let dialog = dialog_id_string(peer);
        let ctx = ctx_with(7011, peer, false);

        let cases: Vec<(LpEvent, Vec<VkServerMsg>)> = vec![
            (
                LpEvent::FlagsReplace {
                    message_id: 1,
                    flags: 0,
                    peer_id: peer,
                },
                vec![],
            ),
            (
                LpEvent::FlagsSet {
                    message_id: 7011,
                    mask: FLAG_DELETED,
                    peer_id: peer,
                },
                vec![VkServerMsg::MessageDeleted {
                    dialog_id: dialog.clone(),
                    message_ids: vec![7011],
                }],
            ),
            (
                // Бит DELETED не установлен, зато сообщение известно —
                // отдаём правку, а не удаление.
                LpEvent::FlagsSet {
                    message_id: 7011,
                    mask: FLAG_OUTBOX,
                    peer_id: peer,
                },
                vec![VkServerMsg::MessageEdited {
                    message: msg(7011, peer),
                }],
            ),
            (
                LpEvent::FlagsReset {
                    message_id: 7011,
                    mask: FLAG_UNREAD,
                    peer_id: peer,
                },
                vec![VkServerMsg::Read {
                    dialog_id: dialog.clone(),
                    max_id: 7011,
                    direction: DIRECTION_IN.into(),
                }],
            ),
            (
                LpEvent::NewMessage {
                    message_id: 7011,
                    flags: 0,
                    peer_id: peer,
                },
                vec![VkServerMsg::NewMessage {
                    message: msg(7011, peer),
                    muted: false,
                    archived: false,
                }],
            ),
            (
                LpEvent::MessageEdited {
                    message_id: 7011,
                    peer_id: peer,
                },
                vec![VkServerMsg::MessageEdited {
                    message: msg(7011, peer),
                }],
            ),
            (
                LpEvent::SnippetAdded {
                    message_id: 7011,
                    peer_id: peer,
                },
                vec![VkServerMsg::MessageEdited {
                    message: msg(7011, peer),
                }],
            ),
            (
                LpEvent::ReadInbox {
                    peer_id: peer,
                    local_id: 40,
                },
                vec![VkServerMsg::Read {
                    dialog_id: dialog.clone(),
                    max_id: 40,
                    direction: DIRECTION_IN.into(),
                }],
            ),
            (
                LpEvent::ReadOutbox {
                    peer_id: peer,
                    local_id: 41,
                },
                vec![VkServerMsg::Read {
                    dialog_id: dialog.clone(),
                    max_id: 41,
                    direction: DIRECTION_OUT.into(),
                }],
            ),
            (
                LpEvent::FriendOnline {
                    user_id: 1234,
                    platform: 7,
                    ts: 1_800_000_000,
                },
                vec![VkServerMsg::Online {
                    user_id: "1234".into(),
                    online: true,
                    was_online: None,
                }],
            ),
            (
                LpEvent::FriendOffline {
                    user_id: 1234,
                    flags: 0,
                    ts: 1_800_000_000,
                },
                vec![VkServerMsg::Online {
                    user_id: "1234".into(),
                    online: false,
                    was_online: Some(1_800_000_000),
                }],
            ),
            (
                LpEvent::DialogFlags {
                    code: 10,
                    peer_id: peer,
                    mask: 8,
                },
                vec![],
            ),
            (
                LpEvent::HistoryCleared {
                    peer_id: peer,
                    up_to_id: 7020,
                },
                vec![VkServerMsg::HistoryCleared {
                    dialog_id: dialog.clone(),
                    up_to_id: 7020,
                }],
            ),
            (
                LpEvent::ChatChanged { peer_id: peer },
                vec![VkServerMsg::DialogChanged {
                    dialog_id: dialog.clone(),
                }],
            ),
            (
                // Одно событие «печатают трое» = ТРИ кадра.
                LpEvent::Typing {
                    peer_id: peer,
                    from_ids: vec![11, 22, 33],
                    action: ACTION_TYPING,
                },
                vec![
                    VkServerMsg::Typing {
                        dialog_id: dialog.clone(),
                        user_id: "11".into(),
                        action: ACTION_TYPING.into(),
                    },
                    VkServerMsg::Typing {
                        dialog_id: dialog.clone(),
                        user_id: "22".into(),
                        action: ACTION_TYPING.into(),
                    },
                    VkServerMsg::Typing {
                        dialog_id: dialog.clone(),
                        user_id: "33".into(),
                        action: ACTION_TYPING.into(),
                    },
                ],
            ),
            (
                LpEvent::Typing {
                    peer_id: peer,
                    from_ids: vec![11],
                    action: ACTION_RECORD_VOICE,
                },
                vec![VkServerMsg::Typing {
                    dialog_id: dialog.clone(),
                    user_id: "11".into(),
                    action: ACTION_RECORD_VOICE.into(),
                }],
            ),
            (LpEvent::Call { user_id: 1234 }, vec![]),
            (
                LpEvent::UnreadTotal { count: 12 },
                vec![VkServerMsg::UnreadTotal { count: 12 }],
            ),
            (
                LpEvent::NotifySettings {
                    peer_id: peer,
                    muted: true,
                },
                vec![VkServerMsg::NotifySettings {
                    dialog_id: dialog.clone(),
                    muted: true,
                }],
            ),
            (LpEvent::Unknown(999), vec![]),
        ];

        for (ev, expected) in cases {
            assert_eq!(map_event(&ev, &ctx), expected, "событие {ev:?}");
        }
    }

    /// Заглушение — свойство БЕСЕДЫ, и решение сервера едет в кадре: фронт не
    /// имеет права решать это по своей загруженной странице списка.
    #[test]
    fn new_message_carries_the_servers_mute_decision() {
        let peer = 1234;
        let ctx = ctx_with(7011, peer, true);
        let frames = map_event(
            &LpEvent::NewMessage {
                message_id: 7011,
                flags: 0,
                peer_id: peer,
            },
            &ctx,
        );
        assert_eq!(
            frames,
            vec![VkServerMsg::NewMessage {
                message: msg(7011, peer),
                muted: true,
                archived: false,
            }]
        );
    }

    /// Обогащение отказало: событие 4 закрывается `resync`, а не карточкой из
    /// урезанных полей, которая «мигнула» бы при reconcile.
    #[test]
    fn message_events_without_enrichment_degrade_to_resync() {
        let empty = MapCtx::empty();
        for ev in [
            LpEvent::NewMessage {
                message_id: 1,
                flags: 0,
                peer_id: 2,
            },
            LpEvent::MessageEdited {
                message_id: 1,
                peer_id: 2,
            },
            LpEvent::SnippetAdded {
                message_id: 1,
                peer_id: 2,
            },
        ] {
            assert_eq!(map_event(&ev, &empty), vec![VkServerMsg::Resync], "{ev:?}");
        }
    }

    /// А вот смена флагов до `resync` НЕ деградирует: события 2/3 приходят
    /// пачками при обычной прокрутке чужой ленты, и `resync` на каждое
    /// означал бы шторм перезагрузок состояния.
    #[test]
    fn flag_events_without_enrichment_stay_silent() {
        let empty = MapCtx::empty();
        assert!(map_event(
            &LpEvent::FlagsSet {
                message_id: 1,
                mask: FLAG_OUTBOX,
                peer_id: 2
            },
            &empty
        )
        .is_empty());
        assert!(map_event(
            &LpEvent::FlagsReset {
                message_id: 1,
                mask: 64,
                peer_id: 2
            },
            &empty
        )
        .is_empty());
    }

    // -------------------------------------------------------------------
    // MapCtx.
    // -------------------------------------------------------------------

    /// Контекст собирается из ответа `getLongPollHistory`: сообщения — по id,
    /// заглушение — по беседам из того же ответа.
    #[test]
    fn map_ctx_is_built_from_the_history_response() {
        let raw: Value = json!({
            "history": [],
            "messages": {"count": 1, "items": [
                {"id": 7011, "peer_id": 2000000001, "date": 1800000000, "from_id": 55, "text": "йо"}
            ]},
            "profiles": [],
            "groups": [],
            "conversations": [
                {"peer": {"id": 2000000001, "type": "chat"}, "push_settings": {"disabled_forever": true}},
                {"peer": {"id": 1234, "type": "user"}}
            ],
            "new_pts": 900
        });
        let hist: LongPollHistoryRaw = serde_json::from_value(raw).unwrap();
        let ctx = MapCtx::from_history(&hist, &NoNames, 1_800_000_000);

        assert_eq!(ctx.message(7011).map(|m| m.text.as_str()), Some("йо"));
        assert!(ctx.is_muted(2_000_000_001));
        assert!(!ctx.is_muted(1234));
        // Незнакомая беседа считается ЗВЕНЯЩЕЙ: молчаливое `true` спрятало бы
        // сообщение, о котором нас просили сообщить.
        assert!(!ctx.is_muted(999));
    }

    /// «Заглушена до» сравнивается с переданным «сейчас», а не с системными
    /// часами: иначе тест на границу зависел бы от даты прогона.
    #[test]
    fn mute_until_is_compared_with_the_injected_now() {
        let raw: Value = json!({
            "conversations": [
                {"peer": {"id": 1, "type": "user"}, "push_settings": {"disabled_until": 1000}}
            ]
        });
        let hist: LongPollHistoryRaw = serde_json::from_value(raw).unwrap();
        assert!(MapCtx::from_history(&hist, &NoNames, 999).is_muted(1));
        assert!(!MapCtx::from_history(&hist, &NoNames, 1000).is_muted(1));
        assert!(!MapCtx::from_history(&hist, &NoNames, 1001).is_muted(1));
    }

    // -------------------------------------------------------------------
    // invalidations.
    // -------------------------------------------------------------------

    /// Из медиа-кэша выбрасываются РОВНО затронутые пары: правка могла
    /// заменить вложение, удаление делает закэшированный файл следом того,
    /// чего уже нет. Новое сообщение не инвалидирует ничего.
    #[test]
    fn invalidations_cover_edits_and_deletes_only() {
        let frames = vec![
            VkServerMsg::NewMessage {
                message: msg(1, 10),
                muted: false,
                archived: false,
            },
            VkServerMsg::MessageEdited { message: msg(2, 10) },
            VkServerMsg::MessageEdited { message: msg(2, 10) },
            VkServerMsg::MessageDeleted {
                dialog_id: "10".into(),
                message_ids: vec![3, 4, 3],
            },
            VkServerMsg::Typing {
                dialog_id: "10".into(),
                user_id: "1".into(),
                action: ACTION_TYPING.into(),
            },
            VkServerMsg::Resync,
        ];
        assert_eq!(
            invalidations(&frames),
            vec![
                ("10".to_string(), 2),
                ("10".to_string(), 3),
                ("10".to_string(), 4),
            ]
        );
        assert!(invalidations(&[]).is_empty());
    }

    // -------------------------------------------------------------------
    // Курсор.
    // -------------------------------------------------------------------

    fn cursor() -> LpCursor {
        LpCursor {
            server: "lp.vk.com/wh123".into(),
            key: "abcdef".into(),
            ts: 1_700_000,
            pts: Some(42),
        }
    }

    /// Круг «записали — прочитали» и права 0600.
    #[test]
    fn cursor_round_trips_through_disk_with_owner_only_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(CURSOR_FILE);
        let now = 1_800_000_000;

        assert!(load_cursor(&path, now).is_none(), "файла ещё нет");
        save_cursor(&path, &cursor(), now);
        assert_eq!(load_cursor(&path, now), Some(cursor()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "ожидались -rw-------, получили {mode:o}");
        }
    }

    /// Протухший курсор (старше суток) выбрасывается ещё при загрузке: иначе
    /// `getLongPollHistory` отвечал бы кодом 10 на КАЖДОМ цикле, и поток не
    /// поднялся бы никогда. Холодного catch-up у VK нет.
    #[test]
    fn stale_cursor_is_dropped_instead_of_looping_on_code_10() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CURSOR_FILE);
        let saved_at = 1_800_000_000;
        save_cursor(&path, &cursor(), saved_at);

        assert!(load_cursor(&path, saved_at + CURSOR_MAX_AGE_SECS).is_some(), "ровно сутки — ещё жив");
        assert!(load_cursor(&path, saved_at + CURSOR_MAX_AGE_SECS + 1).is_none());
        // Часы прыгнули назад (NTP): «курсор из будущего» тоже не берём —
        // его `ts` может относиться к другой сессии LongPoll.
        assert!(load_cursor(&path, saved_at - 1).is_none());
    }

    /// Битый и урезанный файл — норма (диск переполнился на середине записи),
    /// и она обязана давать «начнём с сейчас», а не панику.
    #[test]
    fn broken_cursor_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_800_000_000;
        for (name, body) in [
            ("garbage.json", "{ not json"),
            ("empty.json", ""),
            ("half.json", r#"{"cursor":{"server":"","key":"","ts":0,"pts":null},"saved_at":1800000000}"#),
            ("no_ts.json", r#"{"cursor":{"server":"lp","key":"k","ts":0,"pts":null},"saved_at":1800000000}"#),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            assert!(load_cursor(&path, now).is_none(), "{name}");
        }
    }

    // -------------------------------------------------------------------
    // Тайминги.
    // -------------------------------------------------------------------

    #[test]
    fn backoff_ladder_repeats_the_last_step() {
        let t = LpTimings::default();
        assert_eq!(t.backoff_at(0), Duration::from_secs(1));
        assert_eq!(t.backoff_at(1), Duration::from_secs(2));
        assert_eq!(t.backoff_at(2), Duration::from_secs(5));
        assert_eq!(t.backoff_at(3), Duration::from_secs(10));
        assert_eq!(t.backoff_at(4), Duration::from_secs(30));
        assert_eq!(t.backoff_at(5), Duration::from_secs(60));
        assert_eq!(t.backoff_at(6), Duration::from_secs(60), "последняя повторяется");
        assert_eq!(t.backoff_at(usize::MAX), Duration::from_secs(60));
        assert_eq!(t.last_step(), 5);
    }

    /// `wait_secs` — единственный тайминг, который берётся из конфига.
    #[test]
    fn timings_read_the_wait_from_the_config() {
        let cfg = crate::config::VkConfig {
            longpoll_wait_secs: 7,
            ..crate::config::VkConfig::default()
        };
        assert_eq!(LpTimings::from_config(&cfg).wait_secs, 7);
        // Ноль в конфиге означал бы `a_check` без ожидания, то есть цикл
        // запросов на полной скорости.
        let zero = crate::config::VkConfig {
            longpoll_wait_secs: 0,
            ..crate::config::VkConfig::default()
        };
        assert_eq!(LpTimings::from_config(&zero).wait_secs, 1);
    }

    // -------------------------------------------------------------------
    // exit_for.
    // -------------------------------------------------------------------

    /// Терминальные коды VK ведут в ветки БЕЗ реконнекта; всё остальное —
    /// обычный backoff.
    #[test]
    fn terminal_vk_codes_map_to_worker_exits() {
        let auth = VkError::Api {
            code: CODE_AUTH_FAILED,
            msg: "User authorization failed".into(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        assert!(matches!(exit_for(&auth), Some(WorkerExit::AuthInvalid(_))));

        let scope = VkError::Api {
            code: CODE_ACCESS_DENIED,
            msg: "Access denied".into(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        assert!(matches!(exit_for(&scope), Some(WorkerExit::ScopeLost(_))));

        let captcha = VkError::Api {
            code: CODE_CAPTCHA,
            msg: "Captcha needed".into(),
            captcha_sid: Some("239".into()),
            captcha_img: Some("https://api.vk.com/captcha.php?sid=239".into()),
            redirect_uri: None,
        };
        match exit_for(&captcha) {
            Some(WorkerExit::Captcha { sid, img }) => {
                assert_eq!(sid, "239");
                assert!(img.contains("captcha.php"));
            }
            other => panic!("{other:?}"),
        }

        // Код 14 БЕЗ картинки — не captcha-ветка: показать пользователю
        // нечего, а пауза «до ввода символов» подвесила бы realtime навсегда.
        let blind = VkError::Api {
            code: CODE_CAPTCHA,
            msg: "Captcha needed".into(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        assert!(exit_for(&blind).is_none());

        for other in [
            VkError::Transport {
                reason: "reset".into(),
            },
            VkError::Http { status: 502 },
            VkError::Api {
                code: CODE_INTERNAL_SERVER,
                msg: "Internal error".into(),
                captcha_sid: None,
                captcha_img: None,
                redirect_uri: None,
            },
        ] {
            assert!(exit_for(&other).is_none(), "{other:?}");
        }
    }

    // -------------------------------------------------------------------
    // wait_authorized и backoff_sleep.
    // -------------------------------------------------------------------

    fn state_for_worker_tests() -> Arc<VkState> {
        state_with_auth_poll(Duration::from_millis(20))
    }

    fn state_with_auth_poll(auth_poll: Duration) -> Arc<VkState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::VkConfig {
            cache_dir: dir.path().join("cache"),
            // Заведомо недостижимый адрес: тесты этого блока сеть не трогают,
            // а если бы тронули — упали бы быстро и заметно.
            base_url: "http://127.0.0.1:1/method".into(),
            ..crate::config::VkConfig::default()
        };
        // TempDir живёт до конца процесса: каталог кэша нужен воркеру, а
        // держать его в кадре теста незачем.
        std::mem::forget(dir);
        VkState::with_timings(
            cfg,
            Duration::from_millis(50),
            Duration::from_millis(200),
            LpTimings {
                auth_poll,
                backoff: vec![Duration::from_millis(5), Duration::from_millis(10)],
                wait_secs: 1,
                persist_every: 1,
            },
        )
    }

    /// ГЛАВНЫЙ тест анти-гонки: токен вставляют в тот момент, когда воркер
    /// только-только проверил состояние. `notified().enable()` ДО проверки —
    /// единственное, что не даёт уведомлению пропасть.
    ///
    /// Тик `auth_poll` здесь ЗАВЕДОМО длиннее теста: с коротким тиком воркер
    /// поднимался бы по нему, а не по уведомлению, и тест зеленел бы при
    /// сломанной подписке — то есть не проверял бы ничего.
    #[tokio::test]
    async fn token_inserted_mid_flight_wakes_the_worker() {
        let state = state_with_auth_poll(Duration::from_secs(30));
        let waiter = {
            let st = state.clone();
            tokio::spawn(async move { wait_authorized(&st).await })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        state
            .set_login(crate::login::LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "1".into(),
                    ..Default::default()
                },
            })
            .await;

        let woke = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("воркер не проснулся на вставку токена")
            .unwrap();
        assert!(woke);
    }

    /// ИНВАРИАНТ, ПРОВЕРЯЕМЫЙ ПО ИСХОДНИКУ (тот же приём, что у
    /// `no_source_line_logs_the_exposed_token` в `lib.rs`).
    ///
    /// Тест выше проверяет, что путь «уведомление будит воркера» вообще
    /// работает. Но саму ГОНКУ — «токен вставили между проверкой состояния и
    /// подпиской» — тестом по времени поймать нельзя: окно шириной в
    /// несколько инструкций, между чтением состояния и созданием `notified()`
    /// нет ни одной точки отмены, и на однопоточном рантайме оно не
    /// воспроизводится вовсе. Тест, который «иногда красный», хуже
    /// отсутствующего.
    ///
    /// Поэтому проверяется то, что НАПИСАНО: `enable()` обязан стоять ВЫШЕ
    /// первого `is_authorized()` в теле `wait_authorized`. Переставь их
    /// местами — и этот тест краснеет детерминированно, при любом рантайме.
    #[test]
    fn wait_authorized_subscribes_before_it_checks_the_state() {
        let src = include_str!("longpoll.rs");
        let start = src
            .find("async fn wait_authorized")
            .expect("функция переименована — обнови тест");
        let body = &src[start..];
        let end = body.find("\n}\n").expect("не нашёлся конец функции");
        let body = &body[..end];

        let enable_at = body
            .find(".enable();")
            .expect("подписка `notified().enable()` исчезла из wait_authorized");
        let check_at = body
            .find("is_authorized()")
            .expect("проверка состояния исчезла из wait_authorized");
        assert!(
            enable_at < check_at,
            "notified().enable() обязан стоять ДО is_authorized(): иначе токен, \
             вставленный в этом окне, подвесит воркера до следующего тика"
        );
    }

    /// Shutdown во время ожидания авторизации завершает воркера немедленно —
    /// а не через тик `auth_poll`.
    #[tokio::test]
    async fn shutdown_wins_over_waiting_for_authorization() {
        let state = state_for_worker_tests();
        state.shutdown.cancel();
        let woke = tokio::time::timeout(Duration::from_secs(1), wait_authorized(&state))
            .await
            .expect("wait_authorized завис на отменённом токене");
        assert!(!woke);
    }

    /// Backoff прерывается shutdown'ом: воркер не имеет права держать
    /// остановку процесса минуту.
    #[tokio::test]
    async fn backoff_is_interrupted_by_shutdown() {
        let state = state_for_worker_tests();
        assert!(backoff_sleep(&state, 0).await, "обычная пауза проходит");

        state.shutdown.cancel();
        let started = std::time::Instant::now();
        assert!(!backoff_sleep(&state, 5).await);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "backoff не прервался: {:?}",
            started.elapsed()
        );
    }

    /// Супервизор без токена не делает НИ ОДНОГО сетевого вызова и выходит по
    /// shutdown: именно поэтому его можно запускать всегда.
    #[tokio::test]
    async fn supervisor_without_a_token_idles_and_stops() {
        let state = state_for_worker_tests();
        spawn(&state).await;
        assert_eq!(state.workers.lock().await.len(), 1);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(state.lp_cursor().await.is_none(), "курсор не запрашивался");

        state.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), state.shutdown_workers())
            .await
            .expect("супервизор не вышел по shutdown");
    }
}
