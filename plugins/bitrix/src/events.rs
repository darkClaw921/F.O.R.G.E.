//! Realtime: поток событий мессенджера портала (`im.v2.Event.*`).
//!
//! ```text
//!  ждём вебхук (Notify) ──→ im.v2.Event.subscribe ──→ курсор ──┐
//!         ▲                                                    │
//!         │                        ┌───────────────────────────┘
//!         │                        ▼
//!         │            im.v2.Event.get {offset, limit}
//!         │                        │
//!         │        события → дедуп → кадры → broadcast → WS
//!         │                        │
//!         └──── backoff ◄── ошибка │ успех → пауза по каденсу (Р4)
//! ```
//!
//! # Почему realtime вообще возможен через ВХОДЯЩИЙ вебхук
//!
//! Все привычные механизмы Bitrix24 вебхуку закрыты (проверено живьём):
//! `pull.*` отвечает `insufficient_scope` («работает только в контексте
//! авторизованного приложения»), `event.bind` и `event.get` — `WRONG_AUTH_TYPE`,
//! а исходящие вебхуки требуют публичного HTTPS-адреса, которого у devforge за
//! файрволом нет. Единственная пара, которая работает, — `im.v2.Event.subscribe`
//! плюс polling `im.v2.Event.get`: она задекларирована в документации ровно как
//! режим «для интеграций без постоянного HTTP-сервера».
//!
//! # `offset` — это ПОДТВЕРЖДЕНИЕ, а не позиция чтения
//!
//! Самое неочевидное место всего плагина. Портал держит очередь событий и
//! отдаёт из неё ВСЁ НЕПОДТВЕРЖДЁННОЕ; `offset` в запросе означает «всё, что
//! старше, я обработал». Проверено живьём: вызов с `offset:4` вернул
//! `eventId:5, nextOffset:6`, а повторный вызов БЕЗ `offset` снова вернул
//! `eventId:5`.
//!
//! Отсюда два следствия:
//!
//! 1. **Подтверждать нужно всегда** — иначе каждая пачка приезжает заново, и
//!    лента растёт бесконечно.
//! 2. **Подтверждать нужно ПОСЛЕ обработки.** Курсор двигается только тогда,
//!    когда кадры уже ушли в broadcast; преждевременный ack означал бы, что
//!    упавший между запросом и рассылкой процесс потерял события навсегда —
//!    портал их больше не отдаст.
//!
//! # Дубли реальны, поэтому дедуп обязателен (Р19)
//!
//! Даже при аккуратном ack одно и то же сообщение приходит дважды под разными
//! `eventId` — наблюдалось живьём (`message.id = 61` под `eventId` 1 и 3).
//! Ключ дедупа поэтому `message.id`, а не `eventId`: по `eventId` это честно
//! два разных события. Буфер — кольцевой на [`crate::state::DEDUP_CAPACITY`]
//! записей (см. [`crate::state::EventDedup`]).
//!
//! Дедуп применяется ТОЛЬКО к `ONIMV2MESSAGEADD`: только он создаёт строку
//! ленты. Реакция, удаление и вход в чат идемпотентны по построению (кадр несёт
//! итоговое состояние, а не дельту), и глушить их по идентификатору сообщения
//! значило бы потерять снятие лайка с той строки, которую только что показали.
//!
//! # Ретроспективы нет
//!
//! Очередь начинает копиться с момента `subscribe`. Холодный старт без курсора
//! делает один вызов `get {limit: 1000}` БЕЗ `offset`, берёт `nextOffset` как
//! базу и **отбрасывает сами события**: показать их означало бы вывалить в
//! ленту всё, что случилось, пока devforge был выключен, — историю грузит UI
//! сам, страницами и в правильном порядке.
//!
//! # Чего в этом потоке НЕТ
//!
//! `ONIMV2MESSAGEUPDATE` документирован, но живьём НЕ ПРИХОДИТ ни разу.
//! Обработчик для него объявлен (портал может начать его слать), но полагаться
//! на него нельзя: правки ловит [`crate::recent_sync`] с задержкой до
//! `recent_sync_secs`. Это поведение по проекту, а не баг.
//!
//! # Нотация
//!
//! Полезная нагрузка событий — **camelCase** (`chatId`, `authorId`,
//! `nextOffset`), ответы методов `im.*` — snake_case. Внутри `message` портал
//! дублирует ОБЕ нотации, поэтому строка события проецируется в DTO тем же
//! [`crate::dto::message_from_raw`], что и строка истории.
//!
//! # Каденс (Р4) и лестница backoff
//!
//! | ситуация | пауза |
//! |---|---|
//! | `hasMore: true` | [`HAS_MORE_INTERVAL`] — дренируем очередь |
//! | есть WS-подписчики | `poll_active_secs` (4 с) |
//! | подписчиков нет | `poll_idle_secs` (20 с) |
//! | ошибка | [`BACKOFF_SECS`], сбрасывается первым успехом |
//!
//! Все значения — данные ([`crate::state::PollTimings`]), потому что тест
//! каденса обязан проверять настоящий цикл, а не арифметику вокруг него.

use std::sync::Arc;
use std::time::Duration;

use crate::api::schema::EventRaw;
use crate::api::{B24Client, B24Error};
use crate::dto::PeerNames;
use crate::media_cache;
use crate::state::{B24State, EventCursor};
use crate::ws::protocol::B24ServerMsg;

/// Файл курсора в `<cache_dir>`.
pub const EVENTS_CURSOR_FILE: &str = "events.json";

/// Курсор старше шести часов не заслуживает доверия (Р18).
///
/// Причина не в самом `offset` — он бы сработал, — а в состоянии панели:
/// за полсуток простоя список диалогов, счётчики и открытая лента разъехались
/// с порталом настолько, что догонять их событиями дороже и ненадёжнее, чем
/// перечитать. Поэтому старый курсор приводит к полной пересинхронизации, а не
/// к попытке продолжить с места.
pub const CURSOR_MAX_AGE_SECS: i64 = 6 * 60 * 60;

/// Пауза при `hasMore: true` — дренаж очереди (Р4).
pub const HAS_MORE_INTERVAL: Duration = Duration::from_secs(2);

/// Как часто воркер сам проверяет появление вебхука, если `Notify` не разбудил.
///
/// Страховка от гонки, а не основной механизм: подключение из UI будит воркер
/// немедленно через [`B24State::authorized_notify`].
pub const AUTH_POLL: Duration = Duration::from_secs(2);

/// Лестница пауз после ошибок, секунды. Успех сбрасывает её на первую ступень.
pub const BACKOFF_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// Через сколько успешных циклов курсор пишется на диск.
///
/// Компромисс: писать каждый цикл — это запись в файл каждые 2–4 секунды
/// круглосуточно, не писать вовсе — потеря позиции при `kill -9`. При падении
/// теряется не более десяти пачек, а они переотдадутся порталом (offset — ack)
/// и отсеются дедупом.
pub const CURSOR_PERSIST_EVERY: u32 = 10;

/// Новое сообщение. Полезная нагрузка САМОДОСТАТОЧНА: `message` + `chat` +
/// `user`, обогащающих запросов не требует.
pub const EV_MESSAGE_ADD: &str = "ONIMV2MESSAGEADD";

/// Правка сообщения. **Живьём не приходит** — см. шапку модуля.
pub const EV_MESSAGE_UPDATE: &str = "ONIMV2MESSAGEUPDATE";

/// Удаление сообщения: в событии только `messageId` и `chat`.
pub const EV_MESSAGE_DELETE: &str = "ONIMV2MESSAGEDELETE";

/// Изменение реакции. Поле `action` живьём ПУСТО, поэтому итоговый состав
/// лайков берётся из `message.params.LIKE`.
pub const EV_REACTION_CHANGE: &str = "ONIMV2REACTIONCHANGE";

/// Вход в чат (свой или чужой).
pub const EV_JOIN_CHAT: &str = "ONIMV2JOINCHAT";

// ---------------------------------------------------------------------------
// Проекция события в кадры протокола.
// ---------------------------------------------------------------------------

/// Диалог, которого касается событие, строкой.
///
/// `dialogId` верхнего уровня (его несёт `ONIMV2JOINCHAT`) приоритетнее блока
/// `chat`: они всегда совпадают, но первый — прямое утверждение портала.
pub fn dialog_of(ev: &EventRaw) -> Option<String> {
    ev.data
        .dialog_id
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| ev.data.chat.dialog_id())
}

/// Идентификатор сообщения, по которому работает дедуп (Р19).
///
/// `Some` только у `ONIMV2MESSAGEADD`: см. шапку модуля — прочие типы событий
/// идемпотентны, и глушить их по идентификатору сообщения значило бы терять
/// снятие лайка со строки, показанной секунду назад.
pub fn dedup_key(ev: &EventRaw) -> Option<i64> {
    if ev.kind != EV_MESSAGE_ADD {
        return None;
    }
    ev.data.message.as_ref().map(|m| m.id).filter(|id| *id > 0)
}

/// Превращает событие портала в кадры протокола.
///
/// ЧИСТАЯ функция: ни сети, ни состояния, ни времени — всё, что ей нужно, уже
/// лежит в самом событии либо приходит аргументами. Ровно поэтому она
/// проверяется на живой фикстуре `tests/fixtures/ev.json` без единого запроса.
///
/// Пустой результат — норма: незнакомый тип события не ошибка (портал добавляет
/// их между релизами), а `ONIMV2JOINCHAT` строку списка не несёт и рисуется
/// фолбэк-синхронизацией.
pub fn frames_for(
    ev: &EventRaw,
    dialog_id: &str,
    self_id: i64,
    peers: &dyn PeerNames,
    muted: bool,
) -> Vec<B24ServerMsg> {
    match ev.kind.as_str() {
        EV_MESSAGE_ADD => {
            let Some(raw) = ev.data.message.as_ref() else {
                return Vec::new();
            };
            let message =
                crate::dto::message_from_raw(raw, dialog_id, self_id, &Default::default(), peers);
            vec![B24ServerMsg::MessageNew {
                dialog_id: dialog_id.to_string(),
                message,
                muted,
            }]
        }
        // Объявлен ради полноты: портал его не присылает (см. шапку модуля).
        // Если однажды начнёт — кадр уже тот самый, что шлёт фолбэк-воркер, и
        // фронт разницы не заметит.
        EV_MESSAGE_UPDATE => {
            let Some(raw) = ev.data.message.as_ref() else {
                return Vec::new();
            };
            let message =
                crate::dto::message_from_raw(raw, dialog_id, self_id, &Default::default(), peers);
            vec![B24ServerMsg::MessageUpdate {
                dialog_id: dialog_id.to_string(),
                message,
            }]
        }
        EV_MESSAGE_DELETE => {
            // Идентификатор приходит отдельным полем: самого сообщения в
            // событии нет — оно уже удалено.
            let id = ev
                .data
                .message_id
                .as_i64()
                .or_else(|| ev.data.message.as_ref().map(|m| m.id))
                .unwrap_or_default();
            if id <= 0 {
                return Vec::new();
            }
            vec![B24ServerMsg::MessageDelete {
                dialog_id: dialog_id.to_string(),
                message_ids: vec![id],
            }]
        }
        EV_REACTION_CHANGE => {
            let Some(raw) = ev.data.message.as_ref() else {
                return Vec::new();
            };
            // `action` живьём ПУСТ, поэтому «поставили» и «сняли» различаются
            // только составом `params.LIKE` — его и отдаём целиком, а не
            // дельтой: итоговое состояние идемпотентно, дельта — нет.
            vec![B24ServerMsg::Reaction {
                dialog_id: dialog_id.to_string(),
                message_id: raw.id,
                likes: crate::attach::likes_from_params(&raw.params),
            }]
        }
        _ => Vec::new(),
    }
}

/// Пауза до следующего опроса (Р4).
///
/// Отдельная чистая функция ровно затем, чтобы правило каденса можно было
/// проверить без запуска воркера и без сна.
pub fn next_interval(t: &crate::state::PollTimings, has_more: bool, subscribers: usize) -> Duration {
    if has_more {
        t.has_more
    } else if subscribers > 0 {
        t.active
    } else {
        t.idle
    }
}

// ---------------------------------------------------------------------------
// Курсор на диске.
// ---------------------------------------------------------------------------

/// Путь файла курсора.
pub fn cursor_path(cfg: &crate::config::BitrixConfig) -> std::path::PathBuf {
    cfg.cache_dir.join(EVENTS_CURSOR_FILE)
}

/// Читает сохранённый курсор.
///
/// `None` во всех трёх случаях, когда доверять ему нельзя: файла нет (первый
/// запуск), он битый, курсор старше [`CURSOR_MAX_AGE_SECS`] (Р18) либо его
/// `saved_at` из будущего (переведённые часы). Вызывающий уходит в холодный
/// старт, а признак «был, но протух» возвращается отдельно —
/// [`stored_cursor`].
pub fn load_cursor(path: &std::path::Path, now: i64) -> Option<EventCursor> {
    match stored_cursor(path, now) {
        StoredCursor::Fresh(c) => Some(c),
        StoredCursor::Stale | StoredCursor::Missing => None,
    }
}

/// Что лежит на диске.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredCursor {
    /// Курсор жив — продолжаем поток с него.
    Fresh(EventCursor),
    /// Курсор есть, но старше [`CURSOR_MAX_AGE_SECS`] (Р18): нужна ПОЛНАЯ
    /// пересинхронизация, а не просто новый курсор.
    Stale,
    /// Курсора нет (первый запуск, смена вебхука, битый файл).
    Missing,
}

/// Читает файл курсора и классифицирует находку.
pub fn stored_cursor(path: &std::path::Path, now: i64) -> StoredCursor {
    let Ok(bytes) = std::fs::read(path) else {
        return StoredCursor::Missing;
    };
    let saved: EventCursor = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                target: "forge_bitrix",
                error = %e,
                "forge-bitrix: the events cursor file is unreadable, starting cold"
            );
            return StoredCursor::Missing;
        }
    };
    if saved.offset.trim().is_empty() {
        return StoredCursor::Missing;
    }
    let age = now.saturating_sub(saved.saved_at);
    // Отрицательный возраст — часы переведены назад: доверять такому курсору
    // так же нельзя, как и протухшему.
    if !(0..=CURSOR_MAX_AGE_SECS).contains(&age) {
        tracing::info!(
            target: "forge_bitrix",
            age_secs = age,
            "forge-bitrix: the stored events cursor is stale, resyncing from scratch"
        );
        return StoredCursor::Stale;
    }
    StoredCursor::Fresh(saved)
}

/// Пишет курсор на диск.
///
/// Права `0600` — единообразие с `bitrix.toml`: секрета в файле нет, но
/// раскладывать состояние подключения по машине незачем.
pub fn save_cursor(path: &std::path::Path, cursor: &EventCursor) {
    let Ok(bytes) = serde_json::to_vec(cursor) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = crate::config::write_secret_file(path, &bytes) {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not persist the events cursor"
        );
    }
}

// ---------------------------------------------------------------------------
// Воркер.
// ---------------------------------------------------------------------------

/// Почему цикл опроса завершился.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerExit {
    /// Пришёл сигнал остановки.
    Shutdown,
    /// Подключение перестало быть действительным — ждём нового вебхука.
    Unauthorized,
    /// Всё остальное: повторяем с backoff'ом.
    Failed(String),
}

/// Итог одного захода в цикл.
struct WorkerOutcome {
    /// Почему вышли.
    exit: WorkerExit,
    /// Сколько успешных опросов успело пройти — по ним супервизор решает,
    /// сбрасывать ли лестницу backoff'а: одна долгая сессия с обрывом в конце
    /// не должна уводить следующую попытку сразу на минуту.
    cycles: u64,
}

/// Запускает воркер событий.
///
/// **Вызывается ВСЕГДА, даже без вебхука.** Воркер сам ждёт подключения на
/// [`B24State::authorized_notify`] и до него не делает ни одного сетевого
/// вызова — именно поэтому вставка вебхука в работающем процессе поднимает
/// realtime БЕЗ рестарта devforge.
pub async fn spawn(state: &Arc<B24State>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { supervisor(st).await });
    state.register_worker(handle).await;
    tracing::info!(target: "forge_bitrix", "forge-bitrix: events supervisor spawned");
}

/// Вечный цикл: дождаться подключения → подписаться → крутить опрос.
pub async fn supervisor(state: Arc<B24State>) {
    let mut step = 0usize;
    let last_step = state.poll.last_step();
    // Подписка живёт до смены подключения: `im.v2.Event.subscribe`
    // идемпотентен, но лишний вызов стоит хита лимита интенсивности.
    let mut subscribed = false;

    loop {
        if !wait_authorized(&state).await {
            tracing::info!(target: "forge_bitrix", "forge-bitrix: events supervisor stopped");
            return;
        }

        let client = match state.api_client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "forge_bitrix", error = %e, "forge-bitrix: no API client for the event stream");
                if !backoff_sleep(&state, step).await {
                    return;
                }
                step = (step + 1).min(last_step);
                continue;
            }
        };

        // Курсор ДО подписки: протухший (Р18) обязан сбросить и подписку тоже —
        // иначе после полной пересинхронизации мы продолжили бы подтверждать
        // события в старой позиции очереди.
        let start = match plan_start(&state).await {
            StoredCursor::Fresh(c) => Some(c),
            StoredCursor::Stale => {
                subscribed = false;
                full_resync(&state, &client).await;
                None
            }
            StoredCursor::Missing => None,
        };

        if !subscribed {
            match client.event_subscribe().await {
                Ok(_) => {
                    subscribed = true;
                    tracing::info!(
                        target: "forge_bitrix",
                        "forge-bitrix: subscribed to the portal event stream"
                    );
                }
                Err(e) => {
                    report_error(&state, &e);
                    if unauthorized(&e) {
                        // До нового вебхука ждать бесполезно: следующий виток
                        // заблокируется в wait_authorized.
                        apply_auth_error(&state, &e).await;
                        continue;
                    }
                    if !backoff_sleep(&state, step).await {
                        return;
                    }
                    step = (step + 1).min(last_step);
                    continue;
                }
            }
        }

        let cursor = match start {
            Some(c) => c,
            None => match cold_start(&state, &client).await {
                Ok(c) => c,
                Err(e) => {
                    report_error(&state, &e);
                    if !backoff_sleep(&state, step).await {
                        return;
                    }
                    step = (step + 1).min(last_step);
                    continue;
                }
            },
        };

        let outcome = run_worker(&state, &client, cursor).await;
        if outcome.cycles > 0 {
            step = 0;
        }
        match outcome.exit {
            WorkerExit::Shutdown => {
                tracing::info!(target: "forge_bitrix", "forge-bitrix: events supervisor stopped");
                return;
            }
            WorkerExit::Unauthorized => {
                // Подписка принадлежала прошлому подключению.
                subscribed = false;
                step = 0;
            }
            WorkerExit::Failed(reason) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    reason = %reason,
                    step,
                    "forge-bitrix: the event worker failed"
                );
                if !backoff_sleep(&state, step).await {
                    return;
                }
                step = (step + 1).min(last_step);
            }
        }
    }
}

/// Ждёт подключения плагина к порталу. `false` ⇒ дождались остановки.
///
/// **`notified().enable()` вызывается ДО проверки состояния.** Без этого
/// классическая гонка «вебхук вставили между проверкой и подпиской» подвесила
/// бы воркер до следующего тика: `Notify::notify_waiters` будит только тех, кто
/// уже ждёт, и уведомление, пришедшее в этом окне, пропало бы бесследно.
async fn wait_authorized(state: &Arc<B24State>) -> bool {
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
            () = tokio::time::sleep(state.poll.auth_poll) => {}
        }
    }
}

/// Пауза перед следующей попыткой. `false` ⇒ пришёл shutdown.
async fn backoff_sleep(state: &Arc<B24State>, step: usize) -> bool {
    sleep_or_stop(state, state.poll.backoff_at(step)).await
}

/// Спит, если раньше не пришла остановка. `false` ⇒ пора выходить.
async fn sleep_or_stop(state: &Arc<B24State>, delay: Duration) -> bool {
    tokio::select! {
        biased;
        () = state.shutdown.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// То же, но пауза прерывается ПОЯВЛЕНИЕМ WS-подписчика.
///
/// Применяется только к idle-паузе: она длинная (20 с боевых), и досыпать её
/// до конца после того, как человек открыл панель, — единственная причина, по
/// которой первое сообщение может появиться через треть минуты вместо четырёх
/// секунд. Активный цикл не прерывается намеренно: реконнект нестабильной сети
/// иначе съедал бы бюджет 2 rps.
///
/// `notified().enable()` — ДО ожидания, по той же причине, что и в
/// [`wait_authorized`]: подписчик, появившийся в окне между вызовами, иначе
/// разбудил бы никого.
async fn sleep_or_nudge(state: &Arc<B24State>, delay: Duration) -> bool {
    let nudged = state.poll_nudge.notified();
    tokio::pin!(nudged);
    nudged.as_mut().enable();
    tokio::select! {
        biased;
        () = state.shutdown.cancelled() => false,
        () = &mut nudged => true,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Откуда начинать: из памяти, с диска или с нуля.
async fn plan_start(state: &Arc<B24State>) -> StoredCursor {
    let now = crate::dto::now_unix();
    if let Some(c) = state.event_cursor().await {
        // Курсор в памяти тоже стареет: воркер мог простоять час в backoff'е
        // на упавшей сети.
        let age = now.saturating_sub(c.saved_at);
        if !c.offset.trim().is_empty() && (0..=CURSOR_MAX_AGE_SECS).contains(&age) {
            return StoredCursor::Fresh(c);
        }
        return StoredCursor::Stale;
    }
    let found = stored_cursor(&cursor_path(&state.config), now);
    if let StoredCursor::Fresh(c) = &found {
        state.set_event_cursor(Some(c.clone())).await;
    }
    found
}

/// Полная пересинхронизация панели (Р18).
///
/// Кадр `resync` говорит фронту «перечитай список и открытую ленту по REST», а
/// фолбэк-синхронизация ПОЛНЫМ списком чинит то, чего фронт сам не знает:
/// счётчики, mute и состав диалогов. Порядок важен: сначала команда фронту,
/// потом данные — иначе патч приехал бы клиенту, который через миг всё равно
/// перечитает состояние.
async fn full_resync(state: &Arc<B24State>, client: &Arc<B24Client>) {
    tracing::info!(
        target: "forge_bitrix",
        "forge-bitrix: the events cursor is stale, doing a full resync"
    );
    state.broadcast_event(B24ServerMsg::Resync);
    state.reset_dedup();
    // Отказ фолбэка здесь не фатален: кадр `resync` фронт уже получил и
    // перечитает состояние по REST сам, а список приедет следующим циклом
    // медленного воркера.
    if let Err(e) = crate::recent_sync::sync_once(state, client, None).await {
        tracing::warn!(
            target: "forge_bitrix",
            error_code = e.code().unwrap_or("transport"),
            "forge-bitrix: the resync list reload failed, the frontend will refetch by itself"
        );
    }
}

/// Холодный старт курсора: один `get {limit: 1000}` БЕЗ `offset`.
///
/// События ОТБРАСЫВАЮТСЯ — берётся только `nextOffset`. Ретроспективы у
/// платформы нет, а очередь могла копиться сутками, пока devforge не работал:
/// показать её значило бы залить ленту прошлым (историю грузит UI сам).
///
/// Полученный `offset` сразу же становится курсором и уходит на диск: первый же
/// вызов цикла подтвердит им очередь, и старьё исчезнет у портала.
async fn cold_start(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
) -> Result<EventCursor, B24Error> {
    let page = client
        .events_get(None, crate::api::methods::EVENT_COLD_START_LIMIT)
        .await?;
    let cursor = EventCursor {
        offset: page.next_offset.as_string(),
        saved_at: crate::dto::now_unix(),
    };
    tracing::info!(
        target: "forge_bitrix",
        discarded = page.events.len(),
        offset = %cursor.offset,
        "forge-bitrix: events cold start, the backlog is dropped on purpose"
    );
    state.set_event_cursor(Some(cursor.clone())).await;
    save_cursor(&cursor_path(&state.config), &cursor);
    Ok(cursor)
}

/// Цикл опроса.
async fn run_worker(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    mut cursor: EventCursor,
) -> WorkerOutcome {
    let persist_every = state.poll.persist_every.max(1);
    let mut cycles: u64 = 0;
    let mut since_persist: u32 = 0;

    let exit = loop {
        if state.shutdown.is_cancelled() {
            break WorkerExit::Shutdown;
        }

        let page = tokio::select! {
            biased;
            () = state.shutdown.cancelled() => break WorkerExit::Shutdown,
            r = client.events_get(Some(&cursor.offset), crate::api::methods::EVENT_POLL_LIMIT) => r,
        };

        let page = match page {
            Ok(p) => p,
            Err(e) => {
                report_error(state, &e);
                if unauthorized(&e) {
                    apply_auth_error(state, &e).await;
                    break WorkerExit::Unauthorized;
                }
                break WorkerExit::Failed(e.to_string());
            }
        };

        let delivered = dispatch(state, &page.events).await;

        // Курсор двигается ТОЛЬКО ПОСЛЕ рассылки: `offset` — это
        // подтверждение, и подтвердить непоказанное значит потерять его
        // навсегда.
        let next = page.next_offset.as_string();
        if !next.trim().is_empty() {
            cursor = EventCursor {
                offset: next,
                saved_at: crate::dto::now_unix(),
            };
            state.set_event_cursor(Some(cursor.clone())).await;
        }

        cycles += 1;
        since_persist += 1;
        if since_persist >= persist_every {
            since_persist = 0;
            save_cursor(&cursor_path(&state.config), &cursor);
        }

        if !page.events.is_empty() || delivered > 0 {
            tracing::debug!(
                target: "forge_bitrix",
                events = page.events.len(),
                delivered,
                has_more = page.has_more,
                "forge-bitrix: event batch processed"
            );
        }

        // Каденс выбирается ПО ЧИСЛУ ПОДПИСЧИКОВ (Р4), а idle-пауза вдобавок
        // прерывается их появлением: панель, открытая сразу после ухода в
        // idle, иначе ждала бы первое событие все двадцать секунд.
        let subscribers = state.ws_subscribers();
        let delay = next_interval(&state.poll, page.has_more, subscribers);
        let alive = if subscribers == 0 {
            sleep_or_nudge(state, delay).await
        } else {
            sleep_or_stop(state, delay).await
        };
        if !alive {
            break WorkerExit::Shutdown;
        }
    };

    // Курсор персистится на ЛЮБОЙ ветке выхода: после рестарта поток обязан
    // продолжиться с той же позиции, а не «с сейчас».
    state.set_event_cursor(Some(cursor.clone())).await;
    save_cursor(&cursor_path(&state.config), &cursor);
    WorkerOutcome { exit, cycles }
}

/// Проецирует пачку событий в кадры и рассылает их. Возвращает число кадров.
async fn dispatch(state: &Arc<B24State>, events: &[EventRaw]) -> usize {
    if events.is_empty() {
        return 0;
    }
    let self_id = state.self_id().await;
    let mut sent = 0usize;
    let mut nudge = false;

    for ev in events {
        // Имена приезжают в самом событии — кладём их в кэш ДО проекции,
        // иначе строка ленты подписалась бы числом.
        if let Some(user) = ev.data.user() {
            state.peers.absorb_users(&[user]);
        }

        let Some(dialog_id) = dialog_of(ev) else {
            continue;
        };

        if let Some(mid) = dedup_key(ev) {
            if !state.note_message(mid) {
                tracing::debug!(
                    target: "forge_bitrix",
                    message_id = mid,
                    "forge-bitrix: duplicate event dropped"
                );
                continue;
            }
        }

        if ev.kind == EV_JOIN_CHAT {
            // Строку списка событие не несёт — будим того, кто умеет её
            // построить.
            nudge = true;
            continue;
        }

        let muted = state.is_muted(&dialog_id).await;
        for frame in frames_for(ev, &dialog_id, self_id, &state.peers, muted) {
            invalidate_media(state, &frame).await;
            // Авторазбор голосовых (Ф4). Решение принимается здесь и стоит
            // разбора уже полученного DTO; страница истории (дескрипторы
            // файлов), докачка и движок уезжают в фоновую задачу — поток
            // событий держит ленту всего портала.
            if let B24ServerMsg::MessageNew { message, .. } = &frame {
                crate::voice_auto::on_new_message(state, message).await;
            }
            state.broadcast_event(frame);
            sent += 1;
        }
    }

    if nudge {
        state.sync_nudge.notify_waiters();
    }
    sent
}

/// Стирает кэшированные байты вложений, если строка ленты изменилась (Ф8).
///
/// # Почему это обязательная часть кадра, а не «оптимизация»
///
/// Ответ прокси вложений помечен `Cache-Control: immutable` на год
/// ([`crate::routes::media::CACHE_CONTROL_FILE`]). Правомерно это ровно до тех
/// пор, пока содержимое ключа `(dialog, message_id, index)` действительно
/// неизменно. Удаление и правка сообщения — два случая, когда оно меняется, и
/// оба обязаны стирать запись СЕРВЕРА: иначе `immutable` из корректного
/// заголовка превращается в «показывать вложение удалённого сообщения вечно».
///
/// У удаления есть и вторая половина: портал перестаёт отдавать байты вовсе, и
/// сообщение помечается в [`crate::state::B24State::note_deleted`] — тогда
/// следующий запрос той же карточки отвечает `404` без единого вызова портала.
async fn invalidate_media(state: &Arc<B24State>, frame: &B24ServerMsg) {
    match frame {
        B24ServerMsg::MessageDelete {
            dialog_id,
            message_ids,
        } => {
            for id in message_ids {
                state.note_deleted(*id);
                media_cache::invalidate(&state.config, dialog_id, *id).await;
            }
        }
        // Живьём этот кадр из потока событий не приходит (см. шапку модуля), но
        // ветка обязана быть: если платформа однажды начнёт присылать
        // `ONIMV2MESSAGEUPDATE`, инвалидация не должна оказаться забытой.
        B24ServerMsg::MessageUpdate {
            dialog_id,
            message,
        } => {
            media_cache::invalidate(&state.config, dialog_id, message.id).await;
        }
        _ => {}
    }
}

/// Сообщает панели о фоновой ошибке.
///
/// Троттлинг портала (Р16) уезжает кодом `flood_wait` — фронт ставит по нему
/// отдельную метку в статусбаре: «медленно» и «оборвалось» лечатся по-разному.
/// Обычные транспортные отказы в кадр не превращаются вовсе: воркер повторит их
/// сам по лестнице backoff'а, а мигающая ошибка при живой панели пугает зря.
fn report_error(state: &Arc<B24State>, err: &B24Error) {
    let (code, retry_after_secs) = match err {
        B24Error::Throttled { retry_after_secs } => ("flood_wait", Some(*retry_after_secs)),
        B24Error::OperatingLimit { reset_at } => (
            "flood_wait",
            reset_at.map(|at| (at - crate::dto::now_unix()).max(0) as u64),
        ),
        _ => return,
    };
    state.broadcast_event(B24ServerMsg::Error {
        code: code.to_string(),
        message: "портал ограничил частоту запросов".to_string(),
        retry_after_secs,
    });
}

/// Переводит автомат логина в состояние, соответствующее отказу портала.
///
/// Единая точка отображения «ошибка → экран» живёт в [`crate::login`], и
/// воркер зовёт именно её: собственная ветка здесь означала бы второй набор
/// правил, который разъедется с экраном подключения при первой же правке.
async fn apply_auth_error(state: &Arc<B24State>, err: &B24Error) {
    let login_err = crate::login::LoginError::from_b24(err, crate::dto::now_unix());
    crate::login::apply_login_error(state, login_err).await;
}

/// Отказ означает, что подключение больше не действительно.
fn unauthorized(err: &B24Error) -> bool {
    matches!(
        err.code(),
        Some(crate::api::error::ERR_INVALID_CREDENTIALS)
            | Some(crate::api::error::ERR_NO_AUTH_FOUND)
            | Some(crate::api::error::ERR_INSUFFICIENT_SCOPE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::EventsPageRaw;
    use crate::config::BitrixConfig;
    use crate::dto::NoNames;

    /// Живая выгрузка `im.v2.Event.get` с тестового портала.
    fn live_page() -> EventsPageRaw {
        serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(include_str!("../tests/fixtures/ev.json"))
                .unwrap()["result"]
                .clone(),
        )
        .unwrap()
    }

    /// ГЛАВНАЯ ловушка нотации: у событий camelCase, у методов snake_case.
    /// Промах здесь означал бы `next_offset = 0` на каждом цикле, то есть
    /// бесконечную переотдачу очереди.
    #[test]
    fn the_live_page_parses_with_its_camel_case_notation() {
        let page = live_page();
        assert_eq!(page.next_offset.as_string(), "46");
        assert!(!page.has_more);
        assert_eq!(page.events.len(), 21);

        let first = &page.events[0];
        assert_eq!(first.event_id.as_i64(), Some(5));
        assert_eq!(first.kind, EV_MESSAGE_ADD);
        let msg = first.data.message.as_ref().unwrap();
        assert_eq!(msg.id, 63);
        // Внутри `message` портал дублирует обе нотации, поэтому snake_case
        // поля читаются тем же `MessageRaw`, что и на странице истории.
        assert_eq!(msg.chat_id, 7);
        assert_eq!(msg.author_id, 1);
        assert_eq!(first.data.chat.dialog_id(), Some("chat7".to_string()));
        assert_eq!(first.data.chat.entity_type.as_deref(), Some("TASKS_TASK"));
        assert_eq!(first.data.user().map(|u| u.id.as_string()), Some("1".into()));
    }

    /// ЛОВУШКА PHP: у системного сообщения `user` приезжает пустым МАССИВОМ.
    /// Строгая структура на этом месте роняла бы разбор всей пачки.
    #[test]
    fn an_empty_php_user_is_not_a_user() {
        let page = live_page();
        let system = page
            .events
            .iter()
            .find(|e| e.data.message.as_ref().is_some_and(|m| m.author_id == 0))
            .expect("в фикстуре есть системное сообщение");
        assert!(system.data.user.is_array(), "{:?}", system.data.user);
        assert!(system.data.user().is_none());
    }

    /// Дедуп навешен ТОЛЬКО на новые сообщения: реакция и удаление
    /// идемпотентны, и глушить их по идентификатору строки значило бы терять
    /// снятие лайка.
    #[test]
    fn only_new_messages_take_part_in_the_dedup() {
        let page = live_page();
        let add = page.events.iter().find(|e| e.kind == EV_MESSAGE_ADD).unwrap();
        assert_eq!(dedup_key(add), Some(63));

        let reaction = page
            .events
            .iter()
            .find(|e| e.kind == EV_REACTION_CHANGE)
            .unwrap();
        assert_eq!(dedup_key(reaction), None);
        assert!(reaction.data.message.is_some(), "у реакции есть сообщение");

        let join = page.events.iter().find(|e| e.kind == EV_JOIN_CHAT).unwrap();
        assert_eq!(dedup_key(join), None);
        assert_eq!(dialog_of(join), Some("chat27".to_string()));
    }

    /// Проекция: событие даёт ровно тот же кадр, что и строка истории, — с
    /// именем автора из кэша и признаком «своё».
    #[test]
    fn a_message_event_becomes_the_same_row_as_the_history_page() {
        let page = live_page();
        let ev = page.events.iter().find(|e| e.kind == EV_MESSAGE_ADD).unwrap();
        let frames = frames_for(ev, "chat7", 1, &NoNames, false);
        match &frames[..] {
            [B24ServerMsg::MessageNew {
                dialog_id,
                message,
                muted,
            }] => {
                assert_eq!(dialog_id, "chat7");
                assert_eq!(message.id, 63);
                assert!(message.out, "author_id 1 == self_id 1");
                assert!(!message.system);
                assert_eq!(message.text, "PROBE-B через im.message.add");
                assert!(!muted);
            }
            other => panic!("{other:?}"),
        }

        // Тот же кадр с чужим self_id перестаёт быть «своим».
        match &frames_for(ev, "chat7", 15, &NoNames, true)[..] {
            [B24ServerMsg::MessageNew { message, muted, .. }] => {
                assert!(!message.out);
                assert!(*muted, "mute обязан доехать до фронта кадром");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Реакция: состав лайков берётся из `params.LIKE`, потому что `action`
    /// живьём пуст.
    #[test]
    fn a_reaction_carries_the_whole_like_list_because_action_is_empty() {
        let page = live_page();
        let events: Vec<&EventRaw> = page
            .events
            .iter()
            .filter(|e| e.kind == EV_REACTION_CHANGE)
            .collect();
        assert_eq!(events.len(), 2, "две реакции на одно сообщение");
        assert_eq!(events[0].data.action.as_deref(), Some(""), "action пуст");

        match &frames_for(events[0], "chat27", 1, &NoNames, false)[..] {
            [B24ServerMsg::Reaction {
                message_id, likes, ..
            }] => {
                assert_eq!(*message_id, 79);
                assert_eq!(likes, &vec!["1".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        // Второе событие несёт СНЯТЫЙ лайк — пустым списком, а не дельтой.
        match &frames_for(events[1], "chat27", 1, &NoNames, false)[..] {
            [B24ServerMsg::Reaction { likes, .. }] => assert!(likes.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    /// Удаление несёт только идентификатор: самого сообщения в событии нет.
    #[test]
    fn a_delete_event_needs_no_message_body() {
        let ev: EventRaw = serde_json::from_value(serde_json::json!({
            "eventId": 51,
            "type": EV_MESSAGE_DELETE,
            "data": {"messageId": 95, "chat": {"id": 27, "dialogId": "chat27"}}
        }))
        .unwrap();
        assert_eq!(dialog_of(&ev), Some("chat27".to_string()));
        match &frames_for(&ev, "chat27", 1, &NoNames, false)[..] {
            [B24ServerMsg::MessageDelete {
                dialog_id,
                message_ids,
            }] => {
                assert_eq!(dialog_id, "chat27");
                assert_eq!(message_ids, &vec![95]);
            }
            other => panic!("{other:?}"),
        }

        // Без идентификатора кадра нет вовсе — «удалить неизвестно что» фронт
        // не поймёт, а пустой кадр он бы честно применил ни к чему.
        let empty: EventRaw = serde_json::from_value(serde_json::json!({
            "type": EV_MESSAGE_DELETE, "data": {"chat": {"dialogId": "chat27"}}
        }))
        .unwrap();
        assert!(frames_for(&empty, "chat27", 1, &NoNames, false).is_empty());
    }

    /// Незнакомый тип события — не ошибка: портал добавляет их между релизами.
    #[test]
    fn an_unknown_event_type_is_ignored_quietly() {
        let ev: EventRaw = serde_json::from_value(serde_json::json!({
            "eventId": 99, "type": "ONIMV2SOMETHINGNEW",
            "data": {"chat": {"dialogId": "chat27"}}
        }))
        .unwrap();
        assert!(frames_for(&ev, "chat27", 1, &NoNames, false).is_empty());
    }

    /// Личный диалог адресуется идентификатором СОБЕСЕДНИКА, а не `chat<id>`:
    /// собрать его из числового `chat_id` невозможно.
    #[test]
    fn a_personal_dialog_keeps_its_own_addressing() {
        let ev: EventRaw = serde_json::from_value(serde_json::json!({
            "type": EV_MESSAGE_ADD,
            "data": {"message": {"id": 5, "author_id": 19}, "chat": {"id": 3, "dialogId": "19"}}
        }))
        .unwrap();
        assert_eq!(dialog_of(&ev), Some("19".to_string()));

        // Без `dialogId` остаётся числовая сборка — она верна для групповых.
        let group: EventRaw = serde_json::from_value(serde_json::json!({
            "type": EV_MESSAGE_ADD, "data": {"chat": {"id": 27}}
        }))
        .unwrap();
        assert_eq!(dialog_of(&group), Some("chat27".to_string()));

        // Ни того ни другого — событие без адреса, рассылать его некуда.
        let orphan: EventRaw = serde_json::from_value(serde_json::json!({
            "type": EV_MESSAGE_ADD, "data": {}
        }))
        .unwrap();
        assert_eq!(dialog_of(&orphan), None);
    }

    /// КАДЕНС (Р4) на БОЕВЫХ значениях конфига — именно они уедут в прод.
    #[test]
    fn has_more_shortens_the_interval_to_two_seconds() {
        let t = crate::state::PollTimings::from_config(&BitrixConfig::default());
        assert_eq!(t.active, Duration::from_secs(4));
        assert_eq!(t.idle, Duration::from_secs(20));

        assert_eq!(next_interval(&t, true, 1), Duration::from_secs(2));
        assert_eq!(next_interval(&t, true, 0), Duration::from_secs(2));
        assert_eq!(next_interval(&t, false, 1), Duration::from_secs(4));
        // Ни одного подписчика — панель закрыта, бюджет 2 rps тратить незачем.
        assert_eq!(next_interval(&t, false, 0), Duration::from_secs(20));
    }

    /// Лестница backoff'а — контракт, а не настройка.
    #[test]
    fn the_backoff_ladder_is_frozen_and_saturates() {
        assert_eq!(BACKOFF_SECS, [1, 2, 5, 10, 30, 60]);
        let t = crate::state::PollTimings::from_config(&BitrixConfig::default());
        assert_eq!(t.backoff_at(0), Duration::from_secs(1));
        assert_eq!(t.backoff_at(5), Duration::from_secs(60));
        // Последняя ступень повторяется, а не выходит за границы массива.
        assert_eq!(t.backoff_at(50), Duration::from_secs(60));
        assert_eq!(t.last_step(), 5);
    }

    /// Р18 в чистом виде: возраст курсора решает, продолжаем мы поток или
    /// пересобираем панель целиком.
    #[test]
    fn a_cursor_older_than_six_hours_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EVENTS_CURSOR_FILE);
        assert_eq!(stored_cursor(&path, 1_000_000), StoredCursor::Missing);

        save_cursor(
            &path,
            &EventCursor {
                offset: "46".into(),
                saved_at: 1_000_000,
            },
        );
        assert!(matches!(
            stored_cursor(&path, 1_000_000 + 60),
            StoredCursor::Fresh(c) if c.offset == "46"
        ));
        assert!(matches!(
            stored_cursor(&path, 1_000_000 + CURSOR_MAX_AGE_SECS),
            StoredCursor::Fresh(_)
        ));
        assert_eq!(
            stored_cursor(&path, 1_000_000 + CURSOR_MAX_AGE_SECS + 1),
            StoredCursor::Stale
        );
        // Часы перевели назад — доверять курсору так же нельзя.
        assert_eq!(stored_cursor(&path, 999_000), StoredCursor::Stale);

        // Битый файл — это «курсора нет», а не «паника при старте».
        std::fs::write(&path, "{ не json").unwrap();
        assert_eq!(stored_cursor(&path, 1_000_000), StoredCursor::Missing);
        assert_eq!(load_cursor(&path, 1_000_000), None);
    }

    /// Файл курсора создаётся вместе с каталогом и только для владельца.
    #[test]
    fn the_cursor_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deeper").join(EVENTS_CURSOR_FILE);
        save_cursor(
            &path,
            &EventCursor {
                offset: "7".into(),
                saved_at: 100,
            },
        );
        assert!(path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
    }

    /// Кольцевой буфер: 513-я запись вытесняет первую, а не растёт вечно.
    #[test]
    fn the_dedup_ring_forgets_the_oldest_first() {
        let mut ring = crate::state::EventDedup::new(3);
        assert!(ring.insert(1));
        assert!(!ring.insert(1), "повтор обязан отсеяться");
        assert!(ring.insert(2));
        assert!(ring.insert(3));
        assert!(ring.insert(4));
        assert_eq!(ring.len(), 3);
        assert!(!ring.contains(1), "самый старый вытеснен");
        assert!(ring.contains(4));

        ring.clear();
        assert!(ring.is_empty());
        assert!(ring.insert(1), "после очистки идентификатор снова новый");

        // Боевая ёмкость — ровно Р19.
        assert_eq!(crate::state::DEDUP_CAPACITY, 512);
        let mut big = crate::state::EventDedup::default();
        for id in 0..600 {
            big.insert(id);
        }
        assert_eq!(big.len(), 512);
        assert!(!big.contains(0));
        assert!(big.contains(599));
    }
}
