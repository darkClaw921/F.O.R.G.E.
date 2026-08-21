//! Разделяемое состояние плагина Bitrix24.
//!
//! Создаётся один раз в [`crate::init`], живёт под `Arc` и раздаётся роутам,
//! WS-соединениям и фоновым воркерам.
//!
//! # Что здесь есть и почему
//!
//! | поле | зачем |
//! |---|---|
//! | `config` | неизменяемая часть настроек |
//! | `webhook` | ЕДИНСТВЕННАЯ меняющаяся в рантайме часть конфига (вставка/сброс из UI) |
//! | `broadcast` | поток событий всем WS-подписчикам, **без фильтрации** |
//! | `login` | автомат подключения; переходы строго сериализованы |
//! | `client` | ОДИН API-клиент на секрет ⇒ ОДИН троттлер |
//! | `event_cursor` | ack-курсор `im.v2.Event.get` (Ф7), персистится на диск |
//! | `uploads` | реестр принятых файлов (Ф9), TTL 30 минут |
//! | `health` | транспортное здоровье для 503-ветки роутов |
//! | `workers` + `shutdown` | graceful shutdown с грейсом на персист курсора |
//! | `authorized_notify` | вставка вебхука поднимает realtime БЕЗ рестарта |
//! | `ws_heartbeat`, `ws_idle_timeout` | тайминги — ДАННЫЕ, а не константы: иначе тест idle-таймаута спал бы минуту |
//!
//! # Инвариант секрета
//!
//! Ни одно поле состояния не сериализуемо так, чтобы вебхук утёк наружу:
//! [`crate::config::WebhookUrl`] не реализует `Serialize`, поэтому DTO с
//! секретом физически не соберётся.
//!
//! # Где живёт автомат подключения
//!
//! [`LoginFlow`], [`LoginStateDto`] и [`B24UserDto`] объявлены в
//! [`crate::login`] вместе с `trait LoginBackend`, разбором трёх форм URL и
//! матрицей переходов (Ф3). Здесь они РЕЭКСПОРТИРУЮТСЯ, потому что состояние
//! логина — поле `B24State`, и путь `crate::state::LoginFlow` существовал с
//! Фазы 1: реэкспорт сделал перенос правкой одной строки, не задев ни роутов,
//! ни протокола, ни смоук-тестов хоста.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{BitrixConfig, WebhookUrl};
use crate::ws::protocol::{B24ServerEvent, B24ServerMsg};

pub use crate::login::{B24UserDto, LoginFlow, LoginStateDto};

/// Ёмкость broadcast-канала.
///
/// 1024 кадра — это примерно минута очень активного портала. Подписчик,
/// отставший сильнее, получает `Lagged` и кадр `resync` вместо тихой потери
/// событий.
pub const BROADCAST_CAPACITY: usize = 1024;

/// Время жизни записи о принятом, но не отправленном файле (Ф9).
pub const UPLOAD_TTL_SECS: i64 = 30 * 60;

/// Сколько ждать самостоятельного выхода воркера перед `abort()`.
///
/// Пауза не косметическая: воркер событий на выходе персистит курсор
/// (`events.json`), а `abort()` в произвольной await-точке этого сделать не
/// даст — после рестарта поток событий начался бы «с сейчас».
pub const WORKER_STOP_GRACE: Duration = Duration::from_secs(2);

/// Окно, в течение которого после транспортного отказа соединение ещё
/// считается «падающим» (секунды).
pub const HEALTH_GRACE_SECS: i64 = 3;

/// Тип кэшируемого API-клиента.
///
/// Алиас объявлен отдельной строкой с Фазы 1: поле [`B24State::client`] и его
/// инвариант («один клиент на секрет ⇒ один троттлер; смена секрета сбрасывает
/// кэш в той же функции») существовали до транспорта, а Фаза 2 подставила сюда
/// настоящий тип, не тронув ни одного использования.
pub type CachedApiClient = crate::api::B24Client;

/// Транспортное здоровье связи с порталом.
///
/// Нужно data-роутам: 409 «не подключён» и 503 «портал недоступен» — разные
/// ситуации, и фронт реагирует на них по-разному.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnHealth {
    /// Последний обмен прошёл успешно в момент `at` (unix-время).
    Ok {
        /// Когда.
        at: i64,
    },
    /// Транспорт отказал начиная с `since`.
    Down {
        /// Когда началось.
        since: i64,
        /// Короткая причина (без URL — он содержит секрет).
        reason: String,
    },
}

impl Default for ConnHealth {
    fn default() -> Self {
        Self::Ok { at: 0 }
    }
}

impl ConnHealth {
    /// Считается ли связь живой в момент `now`.
    ///
    /// `false` только ВНУТРИ окна [`HEALTH_GRACE_SECS`] после отказа, дальше
    /// снова `true`. Окно намеренно короткое: оно нужно, чтобы пачка запросов
    /// сразу после обрыва получила честный 503 вместо повторного ожидания
    /// таймаута, а не чтобы «запомнить» отказ — один сетевой чих не должен
    /// закрывать панель до конца дня. Починку подтверждает первый же
    /// успешный обмен ([`B24State::note_ok`]).
    pub fn connected(&self, now: i64) -> bool {
        match self {
            Self::Ok { .. } => true,
            Self::Down { since, .. } => now - *since > HEALTH_GRACE_SECS,
        }
    }

    /// Причина отказа, если окно грейса ещё не истекло.
    pub fn down_reason(&self, now: i64) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::Down { since, reason } if now - *since <= HEALTH_GRACE_SECS => {
                Some(reason.clone())
            }
            Self::Down { .. } => None,
        }
    }
}

/// Ack-курсор потока событий `im.v2.Event.get` (Ф7).
///
/// `offset` у Bitrix24 — это ПОДТВЕРЖДЕНИЕ, а не позиция чтения:
/// неподтверждённые события переотдаются, поэтому курсор обязан пережить
/// рестарт процесса — иначе первый же запуск получил бы всю очередь заново.
///
/// `saved_at` нужен правилу Р18: курсор старше шести часов не заслуживает
/// доверия, и вместо него делается полная перезагрузка списка.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCursor {
    /// Значение `nextOffset` из последнего ответа портала.
    #[serde(default)]
    pub offset: String,
    /// Unix-время, когда курсор был получен.
    #[serde(default)]
    pub saved_at: i64,
}

/// Кольцевой буфер уже показанных `message.id` (Р19).
///
/// # Почему без него лента двоится
///
/// Портал переотдаёт неподтверждённые события и вдобавок кладёт ОДНО И ТО ЖЕ
/// сообщение в очередь под РАЗНЫМИ `eventId` — наблюдалось живьём
/// (`message.id = 61` под `eventId` 1 и 3). Дедупа по `eventId` поэтому
/// недостаточно: он честно считает их двумя событиями, каковыми они и являются.
///
/// # Почему ровно 512
///
/// Буфер защищает от повторов внутри одной сессии опроса, а не хранит историю:
/// пятьсот двенадцать последних идентификаторов — это несколько минут очень
/// активного портала при цикле в 2–4 секунды. Больше — впустую занятая память,
/// меньше — риск, что повтор придёт уже после вытеснения.
///
/// Порядок вытеснения — FIFO по времени ВСТАВКИ, а не по величине
/// идентификатора: события приходят в порядке очереди портала, и «самый старый
/// показанный» — это именно первый вставленный.
#[derive(Debug)]
pub struct EventDedup {
    seen: HashSet<i64>,
    order: VecDeque<i64>,
    capacity: usize,
}

impl Default for EventDedup {
    fn default() -> Self {
        Self::new(DEDUP_CAPACITY)
    }
}

impl EventDedup {
    /// Буфер заданной ёмкости (ноль трактуется как единица — «без дедупа»
    /// состояние существовать не должно).
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Отмечает идентификатор показанным.
    ///
    /// `true` — сообщение ВИДЯТ ВПЕРВЫЕ (его можно отдавать в ленту), `false` —
    /// это повтор.
    pub fn insert(&mut self, id: i64) -> bool {
        if !self.seen.insert(id) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// Знаком ли идентификатор.
    pub fn contains(&self, id: i64) -> bool {
        self.seen.contains(&id)
    }

    /// Сколько идентификаторов помнит буфер.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Пуст ли буфер.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Забывает всё. Зовётся при смене вебхука: чужая переписка — чужие
    /// идентификаторы, и совпадение номеров молча спрятало бы новое сообщение.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

/// Ёмкость кольцевого буфера дедупа (Р19).
pub const DEDUP_CAPACITY: usize = 512;

/// Последнее известное состояние строки списка (Ф7).
///
/// Ровно те четыре поля, по которым фолбэк-синхронизация считает дифф. Полный
/// `DialogDto` здесь был бы копией списка на сервере: он весит на порядок
/// больше, а лишние поля всё равно уезжают клиенту кадром `dialogs_patch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogSeen {
    /// Уведомления заглушены.
    pub muted: bool,
    /// Идентификатор последнего сообщения.
    pub last_id: i64,
    /// Его текст (уже после разбора BB-кодов, обрезанный под превью).
    ///
    /// Хранится ради ОДНОГО вопроса: «тот же идентификатор, другой текст» — это
    /// и есть чужая правка, которую поток событий не приносит.
    pub last_text: String,
    /// Собеседник был в сети (только у личных диалогов).
    pub online: bool,
    /// Идентификатор собеседника — адресат кадра `presence`.
    pub peer_id: i64,
}

/// Тайминги опроса портала (Р4) — ДАННЫЕ, а не константы.
///
/// Ровно та же причина, что у `ws_heartbeat`: тест каденса обязан проверять
/// НАСТОЯЩИЙ цикл воркера, а не арифметику вокруг него, и спать при этом
/// боевые двадцать секунд он не может.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollTimings {
    /// Есть WS-подписчики — панель открыта у кого-то прямо сейчас.
    pub active: Duration,
    /// Подписчиков нет — панель закрыта, бюджет 2 rps тратить незачем.
    pub idle: Duration,
    /// `hasMore: true` — очередь дренируется учащённым опросом.
    pub has_more: Duration,
    /// Период фолбэк-синхронизации списка и счётчиков.
    pub recent_sync: Duration,
    /// Как часто воркер проверяет, не появился ли вебхук, если `Notify`
    /// почему-то не разбудил.
    pub auth_poll: Duration,
    /// Лестница пауз после ошибок.
    pub backoff: Vec<Duration>,
    /// Через сколько успешных циклов курсор пишется на диск.
    pub persist_every: u32,
}

impl Default for PollTimings {
    fn default() -> Self {
        Self::from_config(&BitrixConfig::default())
    }
}

impl PollTimings {
    /// Боевые тайминги из конфига (Р4).
    pub fn from_config(cfg: &BitrixConfig) -> Self {
        Self {
            active: Duration::from_secs(cfg.poll_active_secs),
            idle: Duration::from_secs(cfg.poll_idle_secs),
            has_more: crate::events::HAS_MORE_INTERVAL,
            recent_sync: Duration::from_secs(cfg.recent_sync_secs),
            auth_poll: crate::events::AUTH_POLL,
            backoff: crate::events::BACKOFF_SECS
                .iter()
                .map(|s| Duration::from_secs(*s))
                .collect(),
            persist_every: crate::events::CURSOR_PERSIST_EVERY,
        }
    }

    /// Пауза на ступени `step`; последняя ступень повторяется.
    pub fn backoff_at(&self, step: usize) -> Duration {
        if self.backoff.is_empty() {
            return Duration::from_secs(1);
        }
        self.backoff[step.min(self.backoff.len() - 1)]
    }

    /// Индекс последней ступени лестницы.
    pub fn last_step(&self) -> usize {
        self.backoff.len().saturating_sub(1)
    }
}

/// Запись о принятом, но ещё не отправленном файле (Ф9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadEntry {
    /// Путь staging-файла `<cache_dir>/uploads/<uuid>.part`.
    pub path: std::path::PathBuf,
    /// Имя файла, как его назвал пользователь.
    pub file_name: String,
    /// Размер в байтах.
    pub size: u64,
    /// MIME, определённый по имени.
    pub mime: String,
    /// Unix-время приёма (для TTL).
    pub created_at: i64,
}

/// Состояние плагина.
#[derive(Debug)]
pub struct B24State {
    /// Неизменяемая часть настроек.
    pub config: BitrixConfig,

    /// Действующий вебхук. ЕДИНСТВЕННАЯ меняющаяся в рантайме часть конфига:
    /// пользователь вставляет и сбрасывает его из UI без рестарта процесса.
    pub webhook: RwLock<WebhookUrl>,

    /// Поток событий всем WS-подписчикам.
    ///
    /// **Без фильтрации по активному диалогу**: бейдж непрочитанного и тосты
    /// обязаны работать, когда открыта ЛЮБАЯ вкладка devforge.
    pub broadcast: broadcast::Sender<B24ServerEvent>,

    /// Автомат подключения. Под `Mutex`, чтобы переходы были строго
    /// сериализованы и клиент не увидел их в обратном порядке.
    pub login: Mutex<LoginFlow>,

    /// Кэш API-клиента, ключуемый СЕКРЕТОМ.
    ///
    /// Один клиент на вебхук ⇒ один троттлер: два независимых клиента дали бы
    /// двойной бюджет 2 rps, а портал считает его на пару портал+IP и просто
    /// начал бы рвать соединения.
    pub client: RwLock<Option<(WebhookUrl, Arc<CachedApiClient>)>>,

    /// Курсор потока событий (Ф7).
    pub event_cursor: RwLock<Option<EventCursor>>,

    /// Кольцевой буфер показанных `message.id` (Р19).
    ///
    /// `std::sync::Mutex`, а не `tokio`: внутри нет ни одного `await`, зато
    /// заглядывают в него и воркер событий, и роут отправки — на каждой строке.
    pub event_dedup: std::sync::Mutex<EventDedup>,

    /// Кольцевой буфер УДАЛЁННЫХ `message.id` (Ф8).
    ///
    /// Отдельный от [`Self::event_dedup`], хотя тип тот же: у буферов разный
    /// смысл («уже показывали» против «байтов больше нет») и разный ритм
    /// вытеснения. Общий буфер означал бы, что активная переписка вытесняет
    /// память об удалении, и прокси снова начинал бы ходить в портал за
    /// заведомым отказом. Подробности — [`B24State::note_deleted`].
    pub deleted_messages: std::sync::Mutex<EventDedup>,

    /// Индекс задач: `task_id ↔ chat_id` + матрица прав (Ф10).
    ///
    /// `std::sync::Mutex`, а не `tokio`: внутри нет ни одного `await`, а
    /// заглядывают сюда роуты задач на каждом действии.
    ///
    /// Существует ради одного требования: **действие, запрещённое матрицей
    /// `action`, обязано отвергаться БЕЗ обращения к порталу**. Карточка уже
    /// на экране — значит матрица прочитана, и спрашивать её второй раз, чтобы
    /// ответить «нельзя», означало бы тратить один из двух запросов в секунду
    /// впустую. Подробности — [`crate::tasks::TaskIndex`].
    pub task_index: std::sync::Mutex<crate::tasks::TaskIndex>,

    /// Что плагин в последний раз знал о каждой строке списка (Ф7).
    ///
    /// Наполняется фолбэк-синхронизацией и служит ей же памятью для диффа: без
    /// предыдущего снимка «текст последнего сообщения изменился» отличить от
    /// «пришло новое сообщение» невозможно, а именно на этой разнице держится
    /// доставка ЧУЖИХ ПРАВОК (событие `ONIMV2MESSAGEUPDATE` живьём не
    /// приходит).
    ///
    /// Второй потребитель — воркер событий: `ONIMV2MESSAGEADD` признака mute не
    /// несёт вовсе (`chat.mute_list` есть только в строке списка), а кадр
    /// `message_new` обязан его нести — иначе заглушённый чат всплывал бы
    /// тостом.
    pub dialog_seen: RwLock<HashMap<String, DialogSeen>>,

    /// Тайминги опроса портала (Р4).
    pub poll: PollTimings,

    /// Побудка фолбэк-синхронизации.
    ///
    /// События вида «вас добавили в чат» несут чат, но не строку списка (у неё
    /// другой состав полей), поэтому воркер событий не рисует её сам, а будит
    /// того, кто умеет: список приезжает нормальным `dialogs_patch` в ближайшую
    /// секунду вместо ожидания полного цикла в минуту.
    pub sync_nudge: Notify,

    /// Побудка воркера событий при появлении WS-подписчика.
    ///
    /// # Зачем, если каденс и так смотрит на `ws_subscribers()`
    ///
    /// Смотрит он на него в момент ВЫБОРА паузы, а не во время неё. Панель,
    /// открытая сразу после ухода воркера в idle-сон, ждала бы первое событие
    /// все двадцать секунд — замерено живьём на боевых настройках: 13.8 с до
    /// появления строки в ленте. Побудка делает переход «панель закрыта →
    /// панель открыта» мгновенным, не трогая сам каденс.
    ///
    /// Прерывает ТОЛЬКО idle-сон (см. [`crate::events::run_worker`]): рвать
    /// активный четырёхсекундный цикл на каждом реконнекте значило бы отдать
    /// бюджет 2 rps клиенту с нестабильной сетью.
    pub poll_nudge: Notify,

    /// Кэш имён и присутствия сотрудников портала (Ф4).
    ///
    /// Не под `tokio::Mutex` намеренно: внутри `std::sync::RwLock` и ни одного
    /// `await` — проекция DTO дёргает кэш на КАЖДОЙ строке списка.
    pub peers: crate::peers::PeerCache,

    /// Последний снимок счётчиков портала и время его получения (Ф4).
    ///
    /// Существует ради Р3: холодный старт панели забирает список диалогов и
    /// счётчики ОДНИМ `batch`, и второй запрос ради `GET /api/bitrix/filters`
    /// был бы чистой тратой бюджета 2 rps. Свежесть проверяет
    /// [`B24State::counters_fresh`].
    pub counters: RwLock<Option<(crate::dto::CountersDto, i64)>>,

    /// Снимок счётчиков СОСТОЯНИЯ задач (`tasks.task.counters.get`, Ф10).
    ///
    /// ОТДЕЛЬНАЯ ячейка, а не поля соседней: это второе семейство счётчиков,
    /// которое считается другим методом и обнуляется другим действием (см.
    /// [`crate::tasks::TaskCountersDto`]). Общее хранилище означало бы, что
    /// обновление одного семейства обнуляет другое — а обновляются они
    /// независимо, из разных мест.
    pub task_counters: RwLock<Option<(crate::tasks::TaskCountersDto, i64)>>,

    /// Реестр принятых файлов (Ф9), TTL [`UPLOAD_TTL_SECS`].
    pub uploads: Mutex<HashMap<String, UploadEntry>>,

    /// Дедуп одновременных скачиваний медиа (Ф8):
    /// [`crate::media_cache::CacheKey::id`] → «будильник» ожидающих.
    ///
    /// # Зачем это состояние, а не локальная карта роута
    ///
    /// Прокрутка ленты с десятком вложений порождает десять параллельных
    /// запросов ОДНОГО файла, а бюджет портала — два запроса в секунду. Без
    /// дедупа один экран истории съел бы пятисекундную паузу троттлера и
    /// скачал бы один и тот же файл десять раз.
    ///
    /// Второй потребитель — LRU-подчистка ([`crate::media_cache::sweep_once`]):
    /// файл, который прямо сейчас качается, удалять нельзя, иначе клиент
    /// получит пустоту.
    pub media_inflight: Mutex<HashMap<String, Arc<Notify>>>,

    /// Окно отметок о прочтении (Ф6), [`crate::actions::READ_DEBOUNCE_MS`].
    ///
    /// Живёт в состоянии, а не в роуте, ровно по той же причине, что и
    /// троттлер портала: у бюджета 2 rps один владелец на процесс, и вторая
    /// копия окна означала бы двойной поток `im.dialog.read` при двух открытых
    /// вкладках панели.
    pub read_gate: crate::actions::RateGate,

    /// Окно индикатора «печатает…» (Ф6),
    /// [`crate::actions::TYPING_INTERVAL_MS`]. Отдельный от прочтения: окна
    /// разной длины и подавлять друг друга не должны.
    pub typing_gate: crate::actions::RateGate,

    /// Транспортное здоровье. **Под `Arc`** — ту же ячейку будет держать
    /// `B24Client` (Ф2), чтобы отметка об отказе ставилась там, где отказ
    /// произошёл.
    pub health: Arc<RwLock<ConnHealth>>,

    /// Реестр фоновых воркеров для graceful shutdown.
    pub workers: Mutex<Vec<JoinHandle<()>>>,

    /// Общий сигнал остановки: будит WS-циклы и воркеров.
    pub shutdown: CancellationToken,

    /// Уведомление «вебхук вставлен».
    ///
    /// Воркер событий (Ф7) стартует ВСЕГДА и ждёт здесь, поэтому подключение
    /// портала из UI поднимает realtime БЕЗ рестарта devforge.
    pub authorized_notify: Notify,

    /// Период heartbeat WS. Данные, а не константа: интеграционный тест
    /// поднимает состояние с сотнями миллисекунд и проверяет НАСТОЯЩИЙ цикл,
    /// не дожидаясь боевых 15 секунд.
    pub ws_heartbeat: Duration,

    /// Idle-таймаут WS. Тоже данные — по той же причине.
    pub ws_idle_timeout: Duration,

    // --- «Анализ чата через Claude» ---------------------------------------
    /// Пер-чатовые заметки, последние разборы и треды Q&A
    /// (`forge_chat_analysis::prompts`).
    ///
    /// Под `Arc` — потому что ту же мапу держит [`crate::analysis::AnalysisHub`]:
    /// сохранение разбора по концу run'а идёт под ТЕМ ЖЕ `Mutex`, что и правка
    /// заметки из хендлера, и потому не может её обогнать.
    ///
    /// Ключ мапы — `dialog_id` СТРОКОЙ, в каноническом виде из
    /// [`crate::dto::parse_dialog_id`] (`"chat27"`, `"19"`). У Bitrix он
    /// строкой был изначально — в отличие от Telegram, где `PeerRef`
    /// приходится конвертировать обратно, и от VK с его числовым `peer_id`.
    pub prompts: Arc<Mutex<crate::analysis::prompts::PromptStore>>,

    /// Реестр активных анализов чата ([`crate::analysis::AnalysisHub`]).
    ///
    /// Создаётся ОДИН раз здесь, а не на запрос: в нём живут дедуп по диалогу,
    /// потолок параллелизма и таблица `run_id → процесс`, по которой работает
    /// отмена. Хаб на запрос не знал бы ни о чём из этого.
    pub analysis: crate::analysis::AnalysisHub,
    /// Реестр идущих расшифровок голосовых ([`forge_voice::VoiceHub`]).
    ///
    /// Создаётся безусловно, как и реестр анализа: отсутствие ffmpeg, весов и
    /// ключа облака — отказ КОНКРЕТНОГО запроса со своим кодом, а не повод не
    /// поднять вкладку мессенджера.
    ///
    /// Потолки читаются из `voice.toml` ОДИН раз, в конструкторе: менять их на
    /// лету значило бы бросать уже идущие прогоны. Всё остальное — движок,
    /// язык, ключ — перечитывается на каждый запрос ([`B24State::voice_config`]).
    pub voice: forge_voice::VoiceHub,
    /// Ключи, которые АВТОРАЗБОР уже брал в работу (Ф4).
    ///
    /// Отдельно от дедупа реестра: тот не пускает второй прогон, пока идёт
    /// первый, а стор не даёт пересчитывать готовое, — но прогон, уже
    /// закончившийся отказом, повторная доставка события запускала бы снова.
    pub voice_seen: forge_voice::auto::SeenKeys,
}

impl B24State {
    /// Создаёт состояние из конфига с боевыми таймингами WS.
    pub fn new(config: BitrixConfig) -> Arc<Self> {
        Self::with_ws_timings(config, crate::ws::HEARTBEAT_INTERVAL, crate::ws::IDLE_TIMEOUT)
    }

    /// То же, но с явными таймингами WS — нужен интеграционным тестам.
    pub fn with_ws_timings(
        config: BitrixConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
    ) -> Arc<Self> {
        let poll = PollTimings::from_config(&config);
        Self::with_timings(config, ws_heartbeat, ws_idle_timeout, poll)
    }

    /// То же с явными таймингами ОПРОСА ПОРТАЛА — нужен тестам каденса (Р4):
    /// боевые 4 и 20 секунд превратили бы каждый такой тест в полминуты сна.
    pub fn with_timings(
        config: BitrixConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
        poll: PollTimings,
    ) -> Arc<Self> {
        let (broadcast, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let login = LoginFlow::from_config(&config);
        let webhook = config.webhook.clone();
        // Никаких `?`: `load` не умеет возвращать ошибку по построению —
        // сломанный файл заметок не имеет права мешать плагину подняться.
        let prompts = Arc::new(Mutex::new(crate::analysis::prompts::load(
            &config.prompts_path,
        )));
        // Хаб получает ТУ ЖЕ мапу и тот же путь: сохранение разбора по концу
        // run'а идёт под тем же `Mutex`, что и правка заметки из хендлера.
        // Ядро анализа общее на три мессенджера, поэтому конфиг ему передаётся
        // полями, а не типом: `BitrixConfig` крейту не виден по построению.
        let analysis = crate::analysis::AnalysisHub::new(
            config.claude_cli_path.clone(),
            config.analysis_model.clone(),
            config.analysis_timeout_secs,
        )
        .with_store(prompts.clone(), config.prompts_path.clone());
        // Потолки расшифровки — из `voice.toml`, который лежит рядом с
        // `bitrix.toml` (в бою это `~/.config/forge/`). Битый файл не имеет
        // права мешать плагину подняться: берём встроенные умолчания, а внятный
        // отказ `voice_config` выдаст роут при первом нажатии кнопки.
        let (voice_path, meet_path) = forge_voice::config::paths_next_to(&config.config_path);
        let voice_cfg =
            forge_voice::VoiceConfig::load_from(&voice_path, &meet_path).unwrap_or_default();
        let voice = forge_voice::VoiceHub::new(
            Arc::new(crate::ws::protocol::B24VoiceSink::new(broadcast.clone())),
            voice_cfg.max_parallel,
            voice_cfg.run_timeout(),
        );
        Arc::new(Self {
            config,
            webhook: RwLock::new(webhook),
            broadcast,
            login: Mutex::new(login),
            client: RwLock::new(None),
            event_cursor: RwLock::new(None),
            event_dedup: std::sync::Mutex::new(EventDedup::default()),
            deleted_messages: std::sync::Mutex::new(EventDedup::default()),
            task_index: std::sync::Mutex::new(crate::tasks::TaskIndex::new()),
            dialog_seen: RwLock::new(HashMap::new()),
            poll,
            sync_nudge: Notify::new(),
            poll_nudge: Notify::new(),
            peers: crate::peers::PeerCache::new(),
            counters: RwLock::new(None),
            task_counters: RwLock::new(None),
            uploads: Mutex::new(HashMap::new()),
            media_inflight: Mutex::new(HashMap::new()),
            read_gate: crate::actions::RateGate::new(),
            typing_gate: crate::actions::RateGate::new(),
            health: Arc::new(RwLock::new(ConnHealth::default())),
            workers: Mutex::new(Vec::new()),
            shutdown: CancellationToken::new(),
            authorized_notify: Notify::new(),
            ws_heartbeat,
            ws_idle_timeout,
            prompts,
            analysis,
            voice,
            voice_seen: forge_voice::auto::SeenKeys::default(),
        })
    }

    /// Настройки голосовых сообщений — ЧИТАЮТСЯ ЗАНОВО на каждый запрос.
    ///
    /// `voice.toml` правят руками ровно тогда, когда что-то не работает:
    /// вписывают ключ polza.ai, переключают движок. Конфиг, прочитанный один
    /// раз при старте, означал бы «перезапустите devforge, чтобы настройка
    /// подействовала». Файл маленький, чтение стоит одного `open`.
    ///
    /// Ищется РЯДОМ с `bitrix.toml` — см. [`forge_voice::config::paths_next_to`]:
    /// в бою это `~/.config/forge/`, а в тестах — временный каталог, и изоляция
    /// получается без единой переменной окружения.
    ///
    /// # Errors
    ///
    /// [`forge_voice::VoiceError::Config`] — файл не разбирается; в тексте
    /// назван КОНКРЕТНЫЙ файл (`voice.toml` или `meet.toml`).
    pub fn voice_config(&self) -> Result<forge_voice::VoiceConfig, forge_voice::VoiceError> {
        let (voice, meet) = self.voice_paths();
        forge_voice::VoiceConfig::load_from(&voice, &meet)
    }

    /// Пути `voice.toml` и `meet.toml`, которыми пользуется этот плагин.
    ///
    /// Отдельным методом, потому что их нужно ДВОИМ: чтению настроек и записи
    /// выбранного движка (`PUT …/voice/settings`). Второе написание
    /// `paths_next_to` означало бы, что роут пишет не в тот файл, который
    /// читает состояние, — и заметили бы это только на чужой машине.
    #[must_use]
    pub fn voice_paths(&self) -> (std::path::PathBuf, std::path::PathBuf) {
        forge_voice::config::paths_next_to(&self.config.config_path)
    }

    /// Стор готовых расшифровок рядом с медиа-кэшем плагина.
    ///
    /// Корень — тот же `cache_dir`, что у вложений: расшифровка живёт рядом с
    /// файлом, который расшифровывали, и чистка кэша по диалогу сносит их одним
    /// каталогом.
    #[must_use]
    pub fn voice_store(&self) -> forge_voice::TranscriptStore {
        forge_voice::TranscriptStore::new(&self.config.cache_dir)
    }

    // ------------------------------------------------------------------
    // «Анализ чата через Claude».
    // ------------------------------------------------------------------

    /// Приёмник кадров анализа для [`crate::analysis::AnalysisHub::start`].
    ///
    /// Ядро анализа живёт в общем крейте и про
    /// [`crate::ws::protocol::B24ServerMsg`] не знает: оно зовёт четыре метода
    /// трейта, а [`crate::ws::protocol::B24AnalysisSink`] перекладывает их в
    /// наши кадры и шлёт в тот же broadcast. Хелпер здесь (а не
    /// `Arc::new(...)` в каждом роуте) ради одного: приёмник обязан вещать
    /// ИМЕННО в канал этого состояния, иначе кадры run'а не увидит ни одна
    /// открытая вкладка.
    #[must_use]
    pub fn analysis_sink(&self) -> Arc<dyn crate::analysis::AnalysisSink> {
        Arc::new(crate::ws::protocol::B24AnalysisSink::new(
            self.broadcast.clone(),
        ))
    }

    /// Заметка пользователя к диалогу (снимок, не ссылка).
    ///
    /// Копия, а не guard: держать `Mutex` на время рендера ответа означало бы
    /// сериализовать все запросы вкладки на одном локальном файле.
    pub async fn chat_prompt(
        &self,
        dialog_id: &str,
    ) -> Option<crate::analysis::prompts::ChatPrompt> {
        self.prompts.lock().await.get(dialog_id).cloned()
    }

    /// Записывает заметку к диалогу и СРАЗУ сохраняет файл.
    ///
    /// Мапа и файл меняются под ОДНИМ lock'ом: иначе два одновременных
    /// сохранения (две вкладки) могли бы записать файлы в порядке, обратном
    /// порядку правок, и более старая заметка победила бы. Пустой текст —
    /// удаление записи.
    ///
    /// # Errors
    ///
    /// Ошибка записи файла. Память в этом случае УЖЕ изменена — но источник
    /// истины файл, и следующий успешный `save` его выровняет; откатывать
    /// правку значило бы показать пользователю старый текст в поле, куда он
    /// только что напечатал новый.
    pub async fn set_chat_prompt(
        &self,
        dialog_id: &str,
        raw: &str,
        now: i64,
    ) -> anyhow::Result<Option<crate::analysis::prompts::ChatPrompt>> {
        let mut guard = self.prompts.lock().await;
        let entry = guard.set(dialog_id, raw, now);
        crate::analysis::prompts::save(&self.config.prompts_path, &guard)?;
        Ok(entry)
    }

    /// Последний сохранённый разбор диалога (снимок, не ссылка).
    pub async fn chat_analysis(
        &self,
        dialog_id: &str,
    ) -> Option<crate::analysis::prompts::ChatAnalysis> {
        self.prompts.lock().await.analysis(dialog_id).cloned()
    }

    /// Стирает разбор диалога. Возвращает `true`, если он был.
    ///
    /// Ни заметку, ни тред не трогает: три половины записи независимы — это
    /// три разные кнопки в попапе.
    ///
    /// # Errors
    ///
    /// Ошибка записи файла.
    pub async fn clear_chat_analysis(&self, dialog_id: &str) -> anyhow::Result<bool> {
        let mut guard = self.prompts.lock().await;
        let had = guard.clear_analysis(dialog_id);
        crate::analysis::prompts::save(&self.config.prompts_path, &guard)?;
        Ok(had)
    }

    /// Сохранённый тред вопросов диалога (снимок, не ссылка).
    pub async fn chat_thread(&self, dialog_id: &str) -> Vec<crate::analysis::prompts::QaTurn> {
        self.prompts.lock().await.thread(dialog_id).to_vec()
    }

    /// Стирает тред вопросов диалога («CLEAR Q&A»). Возвращает `true`, если он
    /// был непустым.
    ///
    /// Ни заметку, ни разбор не трогает — см. [`Self::clear_chat_analysis`].
    ///
    /// # Errors
    ///
    /// Ошибка записи файла.
    pub async fn clear_chat_thread(&self, dialog_id: &str) -> anyhow::Result<bool> {
        let mut guard = self.prompts.lock().await;
        let had = guard.clear_thread(dialog_id);
        crate::analysis::prompts::save(&self.config.prompts_path, &guard)?;
        Ok(had)
    }

    // ------------------------------------------------------------------
    // События.
    // ------------------------------------------------------------------

    /// Рассылает событие подписчикам.
    ///
    /// Отсутствие подписчиков — норма (никто не открыл вкладку), поэтому
    /// `SendError` глотается.
    pub fn broadcast_event(&self, ev: B24ServerEvent) {
        let _ = self.broadcast.send(ev);
    }

    /// Подписка на поток событий для нового WS-соединения.
    ///
    /// Заодно БУДИТ воркер событий: подписчик появился — значит, панель открыл
    /// живой человек, и досыпать до конца двадцатисекундной idle-паузы значило
    /// бы показать ему первое сообщение через треть минуты (замерено живьём).
    /// Сам каденс при этом не меняется: следующий цикл выберет интервал по
    /// новому числу подписчиков.
    pub fn subscribe(&self) -> broadcast::Receiver<B24ServerEvent> {
        let rx = self.broadcast.subscribe();
        self.poll_nudge.notify_waiters();
        rx
    }

    /// Сколько сейчас живых WS-подписчиков.
    ///
    /// Воркер событий (Ф7) выбирает по этому числу каденс опроса: ноль ⇒
    /// idle-интервал (Р4).
    pub fn ws_subscribers(&self) -> usize {
        self.broadcast.receiver_count()
    }

    // ------------------------------------------------------------------
    // Подключение.
    // ------------------------------------------------------------------

    /// Снимок состояния подключения для REST/WS.
    pub async fn login_state_dto(&self) -> LoginStateDto {
        self.login.lock().await.to_dto()
    }

    /// Подключён ли плагин к порталу.
    pub async fn is_authorized(&self) -> bool {
        self.login.lock().await.is_authorized()
    }

    /// Идентификатор владельца вебхука либо `0`, если подключения нет.
    ///
    /// Нужен проекции DTO: «своё сообщение» (`out`) и «я заглушил этот чат»
    /// (`muted`) определяются сравнением с ним. Ноль — безопасный ответ:
    /// у портала нет пользователя `0` (этим идентификатором помечены
    /// СИСТЕМНЫЕ сообщения), поэтому без подключения ничего не окажется ни
    /// своим, ни заглушённым.
    pub async fn self_id(&self) -> i64 {
        match &*self.login.lock().await {
            LoginFlow::Authorized { user, .. } => user.id.parse().unwrap_or_default(),
            _ => 0,
        }
    }

    // ------------------------------------------------------------------
    // Счётчики (Ф4).
    // ------------------------------------------------------------------

    /// Запоминает свежий снимок счётчиков.
    pub async fn set_counters(&self, counters: crate::dto::CountersDto, now: i64) {
        *self.counters.write().await = Some((counters, now));
    }

    /// Снимок счётчиков, если он не старше `max_age_secs`.
    ///
    /// `now` приходит параметром, чтобы тест «через минуту» не спал минуту.
    pub async fn counters_fresh(
        &self,
        now: i64,
        max_age_secs: i64,
    ) -> Option<crate::dto::CountersDto> {
        (*self.counters.read().await)
            .filter(|(_, at)| now.saturating_sub(*at) <= max_age_secs)
            .map(|(c, _)| c)
    }

    /// Выбрасывает снимок счётчиков — следующий запрос спросит портал.
    ///
    /// Нужен действиям, которые заведомо МЕНЯЮТ непрочитанное (Ф6: отметка о
    /// прочтении и ручное «непрочитано»). Без сброса бейдж и директории
    /// показывали бы старое число до истечения TTL снимка, то есть до половины
    /// минуты после того, как пользователь своими руками обнулил счётчик.
    ///
    /// Новых значений здесь взять неоткуда: `im.dialog.read` отдаёт счётчик
    /// ОДНОГО диалога, а бейдж собирается из четырёх независимых чисел портала.
    pub async fn invalidate_counters(&self) {
        *self.counters.write().await = None;
        // Второе семейство сбрасывается вместе с первым: действие над задачей
        // двигает и её состояние, и непрочитанное её чата.
        *self.task_counters.write().await = None;
    }

    /// Кладёт снимок счётчиков СОСТОЯНИЯ задач (Ф10).
    pub async fn set_task_counters(&self, counters: crate::tasks::TaskCountersDto, now: i64) {
        *self.task_counters.write().await = Some((counters, now));
    }

    /// Снимок счётчиков задач, если он не старше `max_age_secs`.
    pub async fn task_counters_fresh(
        &self,
        now: i64,
        max_age_secs: i64,
    ) -> Option<crate::tasks::TaskCountersDto> {
        (*self.task_counters.read().await)
            .filter(|(_, at)| now.saturating_sub(*at) <= max_age_secs)
            .map(|(c, _)| c)
    }

    /// Заменяет состояние подключения и рассылает `login_state`.
    ///
    /// Единая точка перехода: broadcast делается под тем же lock'ом, что и
    /// запись, поэтому клиенты не могут увидеть события в порядке, обратном
    /// реальным переходам.
    pub async fn set_login(&self, next: LoginFlow) {
        let authorized = next.is_authorized();
        let dto = {
            let mut guard = self.login.lock().await;
            *guard = next;
            guard.to_dto()
        };
        self.broadcast_event(B24ServerMsg::LoginState(dto));
        if authorized {
            self.authorized_notify.notify_waiters();
        }
    }

    /// Актуальный вебхук.
    pub async fn webhook(&self) -> WebhookUrl {
        self.webhook.read().await.clone()
    }

    /// Задан ли вебхук прямо сейчас.
    ///
    /// Именно этот метод — источник истины для автомата: `config.has_webhook()`
    /// знает только про старт процесса и не видит того, что пользователь
    /// вставил в UI.
    pub async fn has_webhook(&self) -> bool {
        !self.webhook.read().await.is_empty()
    }

    /// Запоминает новый вебхук.
    ///
    /// Кэш клиента сбрасывается ЗДЕСЬ ЖЕ: клиент держит копию секрета внутри,
    /// и оставленный после подключения старый экземпляр продолжил бы ходить в
    /// портал с прошлым вебхуком (после logout — с уже стёртым).
    ///
    /// Вместе с ним обнуляются КУРСОР СОБЫТИЙ и буфер дедупа: новый вебхук —
    /// это другой пользователь портала с другой очередью событий, и
    /// подтверждать ему чужой `offset` бессмысленно, а совпадение номеров
    /// сообщений (они у портала сквозные, но очередь у каждого своя) молча
    /// спрятало бы настоящее новое сообщение.
    pub async fn set_webhook(&self, webhook: WebhookUrl) {
        *self.webhook.write().await = webhook;
        *self.client.write().await = None;
        *self.event_cursor.write().await = None;
        self.dialog_seen.write().await.clear();
        self.reset_dedup();
        // Задачи прошлого вебхука — это задачи ДРУГОГО сотрудника портала:
        // и матрица прав, и сама видимость задачи у него другие (Ф10).
        self.reset_tasks();
    }

    /// Возвращает API-клиент для ТЕКУЩЕГО вебхука, собирая его при первом
    /// обращении.
    ///
    /// # Почему кэш, а не «создать клиент на каждый вызов»
    ///
    /// Вместе с клиентом создаётся троттлер, а троттлер и есть соблюдение
    /// лимита. Два клиента — это два ведра по 2 rps каждое, то есть 4 rps в
    /// портал, который считает бюджет на пару портал+IP. Портал на это
    /// отвечает не ошибкой, а обрывами соединений (§4), поэтому «лишний»
    /// клиент диагностируется мучительно.
    ///
    /// Ключ кэша — сам секрет: смена вебхука обязана дать НОВЫЙ клиент, иначе
    /// после переподключения плагин продолжил бы ходить в портал со старым
    /// (а после logout — со стёртым) кодом. Сброс кэша при записи вебхука
    /// делает [`B24State::set_webhook`]; здесь проверка ключа — вторая линия
    /// на случай, если вебхук поменяли мимо неё.
    ///
    /// # Errors
    ///
    /// Вебхука нет либо он не разбирается (см. [`crate::api::B24Client::new`]).
    /// Текст ошибки не цитирует ни строку вебхука, ни URL.
    pub async fn api_client(&self) -> anyhow::Result<Arc<CachedApiClient>> {
        let webhook = self.webhook().await;
        {
            let guard = self.client.read().await;
            if let Some((cached, client)) = guard.as_ref() {
                if *cached == webhook {
                    return Ok(client.clone());
                }
            }
        }
        let mut guard = self.client.write().await;
        // Повторная проверка под write-lock: пока мы ждали, соседняя задача
        // могла собрать клиент — второй экземпляр означал бы второй троттлер.
        if let Some((cached, client)) = guard.as_ref() {
            if *cached == webhook {
                return Ok(client.clone());
            }
        }
        let client = Arc::new(CachedApiClient::new(
            &self.config,
            &webhook,
            self.health_handle(),
        )?);
        *guard = Some((webhook, client.clone()));
        Ok(client)
    }

    // ------------------------------------------------------------------
    // Здоровье транспорта.
    // ------------------------------------------------------------------

    /// Текущее здоровье.
    pub async fn health(&self) -> ConnHealth {
        self.health.read().await.clone()
    }

    /// Разделяемая ячейка здоровья — её же получит `B24Client` (Ф2).
    pub fn health_handle(&self) -> Arc<RwLock<ConnHealth>> {
        self.health.clone()
    }

    /// Отмечает успешный обмен.
    pub async fn note_ok(&self, now: i64) {
        *self.health.write().await = ConnHealth::Ok { at: now };
    }

    /// Отмечает транспортный отказ.
    ///
    /// Повторный отказ НЕ сдвигает `since`: иначе окно 503 продлевалось бы
    /// вечно и здоровье никогда не чинилось бы.
    pub async fn note_down(&self, now: i64, reason: impl Into<String>) {
        let mut guard = self.health.write().await;
        if let ConnHealth::Down { .. } = &*guard {
            return;
        }
        *guard = ConnHealth::Down {
            since: now,
            reason: reason.into(),
        };
    }

    /// Жива ли связь в момент `now`.
    pub async fn connected(&self, now: i64) -> bool {
        self.health.read().await.connected(now)
    }

    // ------------------------------------------------------------------
    // Курсор событий.
    // ------------------------------------------------------------------

    /// Текущий курсор потока событий.
    pub async fn event_cursor(&self) -> Option<EventCursor> {
        self.event_cursor.read().await.clone()
    }

    /// Ставит курсор потока событий.
    pub async fn set_event_cursor(&self, cursor: Option<EventCursor>) {
        *self.event_cursor.write().await = cursor;
    }

    /// Отмечает сообщение показанным. `true` ⇒ его видят ВПЕРВЫЕ (Р19).
    ///
    /// Зовут двое: воркер событий (перед рассылкой `message_new`) и роут
    /// отправки — он рассылает свою строку сразу, не дожидаясь события, и
    /// обязан закрыть дорогу тому же сообщению, которое через секунду приедет
    /// потоком.
    pub fn note_message(&self, id: i64) -> bool {
        if id <= 0 {
            return false;
        }
        self.dedup_guard().insert(id)
    }

    /// Знаком ли уже этот `message.id`.
    pub fn seen_message(&self, id: i64) -> bool {
        self.dedup_guard().contains(id)
    }

    /// Забывает показанные идентификаторы (смена вебхука).
    pub fn reset_dedup(&self) {
        self.dedup_guard().clear();
        self.deleted_guard().clear();
    }

    // ------------------------------------------------------------------
    // Индекс задач (Ф10).
    // ------------------------------------------------------------------

    /// Запоминает связь `task_id ↔ chat_id` и матрицу прав задачи.
    ///
    /// Пустая матрица (строка `tasks.task.list`, где `action` не приходит
    /// вовсе) НЕ затирает уже известную — см. [`crate::tasks::TaskIndex::remember`].
    pub fn remember_task(
        &self,
        task_id: i64,
        chat_id: Option<i64>,
        actions: crate::tasks::ActionMatrix,
        now: i64,
    ) {
        self.task_guard().remember(task_id, chat_id, actions, now);
    }

    /// Что известно о задаче прямо сейчас.
    pub fn task_entry(&self, task_id: i64) -> Option<crate::tasks::TaskEntry> {
        self.task_guard().get(task_id).cloned()
    }

    /// Задача открытого чата — обратное направление индекса.
    pub fn task_of_chat(&self, chat_id: i64) -> Option<i64> {
        self.task_guard().task_of_chat(chat_id)
    }

    /// Забывает все задачи (смена вебхука, logout).
    pub fn reset_tasks(&self) {
        self.task_guard().clear();
    }

    /// Блокировка индекса задач с восстановлением после отравления.
    ///
    /// Ровно та же политика, что у буферов дедупа: паника в чужом потоке не
    /// должна выключать задачи до перезапуска процесса.
    fn task_guard(&self) -> std::sync::MutexGuard<'_, crate::tasks::TaskIndex> {
        self.task_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Помечает сообщение УДАЛЁННЫМ (Ф8).
    ///
    /// # Зачем плагину помнить удаления
    ///
    /// Портал удаление не стирает: он заменяет текст заглушкой и ставит
    /// `params.IS_DELETED: "Y"`, а вот БАЙТЫ вложения после этого не отдаёт
    /// вовсе — `im.v2.File.download` начинает отвечать отказом. Без памяти о
    /// таком сообщении карточка в старой ленте на каждый показ стоила бы двух
    /// вызовов портала (страница истории + отказ скачивания) и всё равно
    /// заканчивалась бы `404`.
    ///
    /// С памятью тот же запрос стоит НУЛЯ вызовов: ответ `404` уходит
    /// мгновенно. Зовут двое: воркер событий на `ONIMV2MESSAGEDELETE` и сам
    /// роут медиа, когда увидел `deleted: true` в свежей странице истории.
    ///
    /// Буфер кольцевой ([`DEDUP_CAPACITY`]): вытеснение старой записи не
    /// ошибка, а возврат к обычному пути «спросить портал».
    pub fn note_deleted(&self, id: i64) -> bool {
        if id <= 0 {
            return false;
        }
        self.deleted_guard().insert(id)
    }

    /// Известно ли, что сообщение удалено.
    pub fn is_deleted(&self, id: i64) -> bool {
        self.deleted_guard().contains(id)
    }

    /// Доступ к буферу удалённых, переживающий отравленный lock.
    fn deleted_guard(&self) -> std::sync::MutexGuard<'_, EventDedup> {
        match self.deleted_messages.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Доступ к буферу, переживающий отравленный lock.
    ///
    /// Отравление означает панику в другом потоке ПОД этим же lock'ом. Ронять
    /// из-за неё весь realtime незачем: данные производные, и худшее, что
    /// грозит, — одна повторная строка в ленте.
    fn dedup_guard(&self) -> std::sync::MutexGuard<'_, EventDedup> {
        match self.event_dedup.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Заглушён ли диалог (по последнему известному списку).
    pub async fn is_muted(&self, dialog_id: &str) -> bool {
        self.dialog_seen
            .read()
            .await
            .get(dialog_id)
            .is_some_and(|d| d.muted)
    }

    /// Что мы в последний раз знали об этой строке списка.
    pub async fn seen_dialog(&self, dialog_id: &str) -> Option<DialogSeen> {
        self.dialog_seen.read().await.get(dialog_id).cloned()
    }

    /// Запоминает состояние строк списка.
    ///
    /// `full == true` (полная перезагрузка) сначала забывает всё: строка,
    /// исчезнувшая из полного ответа, действительно исчезла (её скрыли или
    /// покинули). При инкрементальной синхронизации так делать нельзя —
    /// отсутствие строки в ответе означает лишь «она не менялась».
    pub async fn remember_dialogs(&self, rows: Vec<(String, DialogSeen)>, full: bool) {
        let mut guard = self.dialog_seen.write().await;
        if full {
            guard.clear();
        }
        for (id, seen) in rows {
            guard.insert(id, seen);
        }
    }

    // ------------------------------------------------------------------
    // Воркеры.
    // ------------------------------------------------------------------

    /// Регистрирует фонового воркера для последующего graceful shutdown.
    pub async fn register_worker(&self, handle: JoinHandle<()>) {
        let mut guard = self.workers.lock().await;
        // Отработавшие ручки выбрасываются ЗДЕСЬ, а не при остановке. До Ф4
        // список пополнялся считаные разы за жизнь процесса (лонгпол, sweeper,
        // GC загрузок), и расти ему было некуда. Авторазбор голосовых
        // регистрирует ЗАДАЧУ НА СООБЩЕНИЕ — без подчистки открытая на неделю
        // панель копила бы по ручке на каждое голосовое аккаунта.
        guard.retain(|h| !h.is_finished());
        guard.push(handle);
    }

    /// Останавливает всех зарегистрированных воркеров.
    ///
    /// Сначала даёт каждому [`WORKER_STOP_GRACE`] выйти самому по
    /// `shutdown`-токену и только потом абортит.
    ///
    /// Идемпотентно: повторный вызов просто не найдёт handle'ов.
    pub async fn shutdown_workers(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut guard = self.workers.lock().await;
            std::mem::take(&mut *guard)
        };
        for mut h in handles {
            if tokio::time::timeout(WORKER_STOP_GRACE, &mut h).await.is_err() {
                h.abort();
                let _ = h.await;
            }
        }
    }

    // ------------------------------------------------------------------
    // Реестр загрузок (Ф9).
    // ------------------------------------------------------------------

    /// Кладёт запись о принятом файле.
    pub async fn insert_upload(&self, upload_id: String, entry: UploadEntry) {
        self.uploads.lock().await.insert(upload_id, entry);
    }

    /// ПОТРЕБЛЯЕТ запись: возвращает её и убирает из реестра.
    ///
    /// Именно поэтому повторная отправка с тем же `upload_id` невозможна:
    /// второй запрос уже не найдёт записи и получит `409 invalid_state`
    /// вместо дубля сообщения.
    pub async fn take_upload(&self, upload_id: &str) -> Option<UploadEntry> {
        self.uploads.lock().await.remove(upload_id)
    }

    /// Выбрасывает записи старше `ttl_secs` и возвращает их.
    ///
    /// Удаление staging-файлов — забота вызывающего: состояние не делает
    /// файловый ввод-вывод, чтобы его можно было тестировать без ФС. `now`
    /// передаётся снаружи, чтобы тест «через 30 минут» не спал полчаса.
    pub async fn evict_stale_uploads(&self, ttl_secs: i64, now: i64) -> Vec<UploadEntry> {
        let mut guard = self.uploads.lock().await;
        let stale: Vec<String> = guard
            .iter()
            .filter(|(_, e)| now - e.created_at > ttl_secs)
            .map(|(k, _)| k.clone())
            .collect();
        stale.iter().filter_map(|k| guard.remove(k)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BitrixConfig {
        BitrixConfig {
            cache_dir: std::env::temp_dir().join("forge-bitrix-tests"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: std::env::temp_dir().join("forge-bitrix-tests").join("prompts.json"),
            claude_cli_path: std::env::temp_dir().join("forge-bitrix-tests").join("no-such-claude"),
            ..BitrixConfig::default()
        }
    }

    #[tokio::test]
    async fn initial_login_state_follows_the_config() {
        let st = B24State::new(cfg());
        assert_eq!(st.login_state_dto().await.state, "not_configured");

        let st = B24State::new(BitrixConfig {
            webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/"),
            ..cfg()
        });
        assert_eq!(st.login_state_dto().await.state, "disconnected");
        assert!(st.has_webhook().await);
    }

    /// Секрет не может попасть в снимок состояния даже случайно: DTO
    /// сериализуется, а `WebhookUrl` в нём отсутствует по типам.
    #[tokio::test]
    async fn login_state_dto_never_carries_the_webhook() {
        let st = B24State::new(BitrixConfig {
            webhook: WebhookUrl::new("https://test.bitrix24.ru/rest/1/supersecretcode/"),
            ..cfg()
        });
        let json = serde_json::to_string(&st.login_state_dto().await).unwrap();
        assert!(!json.contains("supersecretcode"), "{json}");
        assert!(!json.contains("test.bitrix24.ru"), "{json}");
    }

    #[tokio::test]
    async fn set_webhook_resets_the_client_cache() {
        let st = B24State::new(BitrixConfig {
            webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/aaa111"),
            ..cfg()
        });
        let first = st.api_client().await.unwrap();
        assert!(st.client.read().await.is_some(), "клиент обязан закэшироваться");

        st.set_webhook(WebhookUrl::new("https://p.bitrix24.ru/rest/1/bbb111"))
            .await;
        assert!(st.client.read().await.is_none(), "кэш клиента не сброшен");
        assert!(st.has_webhook().await);

        // Новый секрет — НОВЫЙ клиент, иначе плагин продолжил бы ходить в
        // портал с прошлым кодом.
        let second = st.api_client().await.unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }

    /// Один клиент на секрет ⇒ один троттлер: параллельные обращения обязаны
    /// получить ОДИН И ТОТ ЖЕ экземпляр.
    #[tokio::test]
    async fn api_client_is_built_once_per_secret() {
        let st = B24State::new(BitrixConfig {
            webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/aaa111"),
            ..cfg()
        });

        let mut handles = Vec::new();
        for _ in 0..8 {
            let st = st.clone();
            handles.push(tokio::spawn(async move { st.api_client().await.unwrap() }));
        }
        let mut clients = Vec::new();
        for h in handles {
            clients.push(h.await.unwrap());
        }
        for c in &clients[1..] {
            assert!(
                Arc::ptr_eq(&clients[0], c),
                "второй клиент = второй троттлер = двойной бюджет запросов"
            );
        }
    }

    /// Без вебхука клиента нет — и ошибка не цитирует ни строку, ни URL.
    #[tokio::test]
    async fn api_client_without_a_webhook_fails_without_leaking() {
        let st = B24State::new(cfg());
        let err = st.api_client().await.unwrap_err().to_string();
        assert!(!err.contains("rest/"), "{err}");
    }

    #[tokio::test]
    async fn set_login_broadcasts_and_notifies() {
        let st = B24State::new(cfg());
        let mut rx = st.subscribe();
        st.set_login(LoginFlow::Checking).await;
        match rx.recv().await.unwrap() {
            B24ServerMsg::LoginState(dto) => assert_eq!(dto.state, "checking"),
            other => panic!("ожидался login_state, пришло {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_grace_window_hides_a_single_hiccup() {
        let st = B24State::new(cfg());
        assert!(st.connected(100).await);

        st.note_down(100, "connect timeout").await;
        assert!(!st.connected(101).await, "внутри грейса роуты отдают 503");
        assert_eq!(
            st.health().await.down_reason(101).as_deref(),
            Some("connect timeout")
        );
        assert!(
            st.connected(200).await,
            "за грейсом даём порталу второй шанс, а не помним отказ вечно"
        );
        assert!(st.health().await.down_reason(200).is_none());

        // Повторный отказ не сдвигает `since`, иначе окно 503 не кончилось бы
        // никогда.
        st.note_down(150, "connect timeout").await;
        match st.health().await {
            ConnHealth::Down { since, .. } => assert_eq!(since, 100),
            other => panic!("ожидался Down, пришло {other:?}"),
        }

        st.note_ok(300).await;
        assert!(st.connected(300).await);
    }

    #[tokio::test]
    async fn stale_uploads_are_evicted_without_sleeping() {
        let st = B24State::new(cfg());
        st.insert_upload(
            "a".into(),
            UploadEntry {
                path: "/tmp/a.part".into(),
                file_name: "a.txt".into(),
                size: 1,
                mime: "text/plain".into(),
                created_at: 0,
            },
        )
        .await;
        st.insert_upload(
            "b".into(),
            UploadEntry {
                path: "/tmp/b.part".into(),
                file_name: "b.txt".into(),
                size: 1,
                mime: "text/plain".into(),
                created_at: 10_000,
            },
        )
        .await;

        let evicted = st.evict_stale_uploads(UPLOAD_TTL_SECS, 10_000).await;
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].file_name, "a.txt");
        assert!(st.take_upload("b").await.is_some());
        // Запись ПОТРЕБЛЕНА: повторная отправка того же файла невозможна.
        assert!(st.take_upload("b").await.is_none());
    }

    #[tokio::test]
    async fn ws_subscribers_counts_live_receivers() {
        let st = B24State::new(cfg());
        assert_eq!(st.ws_subscribers(), 0);
        let rx = st.subscribe();
        assert_eq!(st.ws_subscribers(), 1);
        drop(rx);
        assert_eq!(st.ws_subscribers(), 0);
    }
}
