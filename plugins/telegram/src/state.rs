//! Разделяемое состояние плагина Telegram.
//!
//! [`TelegramState`] создаётся один раз в [`crate::init`], оборачивается в
//! `Arc` и раздаётся axum-хендлерам через `with_state`, WS-соединениям и
//! фоновым воркерам. Внутри — только примитивы синхронизации tokio,
//! поэтому структура `Send + Sync` и переживает любое количество клонов
//! `Arc`.
//!
//! ## Почему такой набор полей
//!
//! - `broadcast` (ёмкость 1024) — единственный путь событий к WS-клиентам.
//!   Фильтрации по активному диалогу НЕТ: unread-бейдж и тосты должны
//!   работать с любой вкладки, поэтому сервер вещает всё, а решает фронт.
//! - `login` под `Mutex` — переходы автомата строго сериализованы, иначе
//!   два параллельных `login/code` могли бы затереть токен друг друга.
//! - `runtime` под `RwLock` — читается на каждом API-вызове (частое),
//!   пишется только при подключении/отключении (редкое).
//! - `uploads` — реестр незавершённых загрузок; запись живёт до
//!   [`UPLOAD_TTL_SECS`], после чего её подчищает воркер (иначе
//!   оборванная вкладка навсегда оставляла бы staging-файл).
//! - `media_inflight` — дедупликация одновременных скачиваний одного и
//!   того же медиа: второй запрос ждёт `Notify` вместо повторной качки.
//! - `shutdown` + `workers` — graceful shutdown по образцу forge-echo.
//! - `notify_defaults` / `notify_overrides` / `archived_ids` — серверный
//!   источник истины о том, «звенит» ли диалог (см. блок ниже).
//!
//! ## Почему mute и архив кэшируются на сервере
//!
//! Фронт видит только ЗАГРУЖЕННУЮ страницу списка диалогов (сотня верхних),
//! а WS-события прилетают от любого чата аккаунта. Пока решение «показывать
//! ли тост» принимал фронт по своему неполному списку, любое сообщение из
//! чата за пределами страницы считалось незаглушённым — ровно та жалоба,
//! из-за которой появились эти три поля.
//!
//! Поэтому сервер держит:
//! - [`TelegramState::notify_defaults`] — глобальные дефолты трёх категорий
//!   Telegram (`account.getNotifySettings` с `InputNotifyUsers` /
//!   `InputNotifyChats` / `InputNotifyBroadcasts`). Без них «заглушить все
//!   личные чаты» с телефона вообще невидимо: у самих диалогов
//!   `PeerNotifySettings` приезжает пустым и решение принимается по дефолту
//!   категории;
//! - [`TelegramState::notify_overrides`] — уже РАЗРЕШЁННЫЙ (с учётом
//!   наследования) `muted` по диалогам, которые мы видели: заполняется при
//!   листинге диалогов и по `updateNotifySettings`;
//! - [`TelegramState::archived_ids`] — какие диалоги лежат в архиве, чтобы
//!   не «звенеть» архивом даже для чата, которого фронт никогда не грузил.
//!
//! Ключ обоих кэшей — `i64` в формате Bot API (тот же, что в `dialog_id`
//! строкой), см. [`dialog_key`]. Кэши намеренно ДОПОЛНЯЮТСЯ, а не
//! пересеваются целиком: списки диалогов приходят страницами (основной
//! список, архив, папки), и полная замена стирала бы знание о чатах из
//! соседних страниц — то есть возвращала бы исходный дефект.
//!
//! ## Промах кэша флагов: [`TelegramState::flags_probed`]
//!
//! Дополнение страницами оставляет дыру: чат, замьюченный ТОЧЕЧНО и не
//! попавший ни в одну загруженную страницу, кэшу неизвестен, и потребитель
//! вынужден брать дефолт категории — то есть считать его звенящим. Закрывает
//! дыру ленивый резолв ([`crate::updates::probe_dialog_flags`]): один
//! `messages.getPeerDialogs` на диалог. Чтобы этот резолв не повторялся на
//! каждом сообщении из чата, который Telegram нам отдавать отказывается,
//! попытка «застолбляется» в [`TelegramState::flags_probed`] — не более
//! одного RPC на диалог за сессию.
//!
//! ## Зачем [`TelegramState::dialog_kinds`]
//!
//! Категория уведомлений («личные»/«группы»/«каналы») по одному лишь
//! `PeerId` не выводится: мегагруппа и broadcast-канал — оба
//! `PeerKind::Channel`. Пока категорию угадывали по тегу id, мегагруппа с
//! УНАСЛЕДОВАННЫМИ настройками получала дефолт каналов: у пользователя
//! заглушены «все группы», а чат звенел. Настоящий `kind` известен там, где
//! есть данные чата (листинг диалогов, ответ `messages.getPeerDialogs`), —
//! оттуда он и запоминается.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use grammers_client::Client;
use grammers_session::types::PeerId;
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::TelegramConfig;
use crate::dto::{self, DialogDto, NotifyDefaults};
use crate::login::{LoginFlow, LoginStateDto};
use crate::ws::protocol::{TgServerEvent, TgServerMsg};

/// Ёмкость broadcast-канала. 1024 кадра — запас на всплеск апдейтов
/// (массовая пересылка, догон истории после реконнекта) без отставания
/// нормального клиента. Отставший получает `Lagged` ⇒ WS шлёт `resync`.
pub const BROADCAST_CAPACITY: usize = 1024;

/// Сколько живёт запись незавершённой загрузки, секунды.
pub const UPLOAD_TTL_SECS: i64 = 30 * 60;

/// Сколько ждать самостоятельного выхода воркера при shutdown, прежде чем
/// оборвать его `abort()` (см. [`TelegramState::shutdown_workers`]).
pub const WORKER_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

pub use crate::runtime::TgRuntime;

/// Ссылка на peer'а (диалог/пользователь/канал) в терминах MTProto:
/// идентичность (`PeerId`) + право обращаться к нему (`PeerAuth`, он же
/// `access_hash`).
///
/// Это ТОТ ЖЕ тип, что принимают методы grammers (`send_message`,
/// `iter_messages`, `action`), поэтому кэш `peers` отдаёт значения,
/// пригодные для вызовов без дополнительных преобразований.
pub use grammers_session::types::PeerRef;

/// Запись реестра незавершённых загрузок.
///
/// Заполняется при `POST /api/telegram/upload` (браузер → staging-файл на
/// диске) и потребляется при отправке сообщения с `upload_id`.
///
/// Хранится именно ПУТЬ, а не готовый `Uploaded`-хендл Telegram: заливка в
/// Telegram делается на шаге отправки сообщения, потому что только тогда
/// фронт уже знает `upload_id` и может связать с карточкой события
/// `upload_progress`. Побочная выгода — файл, который пользователь выбрал и
/// передумал отправлять, вообще не уходит в сеть.
#[derive(Debug, Clone)]
pub struct UploadEntry {
    /// Путь staging-файла на диске.
    pub path: std::path::PathBuf,
    /// Исходное имя файла (для `X-Forge-File-Name` и подписи в Telegram).
    pub file_name: String,
    /// Размер в байтах.
    pub size: u64,
    /// MIME по расширению имени — по нему выбирается photo vs file.
    pub mime: String,
    /// Unix-время создания записи — база для [`UPLOAD_TTL_SECS`].
    pub created_at: i64,
}

/// Креды приложения с <https://my.telegram.org>.
///
/// Вынесены из [`TelegramConfig`] в отдельное поле под `RwLock`, потому
/// что это ЕДИНСТВЕННАЯ часть конфигурации, которая меняется во время
/// работы процесса: пользователь вводит `api_id`/`api_hash` в
/// login-wizard'е, и после `save_credentials` они должны стать видны
/// всем — иначе `build_runtime` продолжал бы работать со старым (пустым)
/// снапшотом конфига.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCredentials {
    /// `api_id` приложения.
    pub api_id: i32,
    /// `api_hash` приложения — секрет, в логи не попадает.
    pub api_hash: String,
}

/// `dialog_id` строкой → ключ кэшей mute/архива (`i64` формата Bot API).
///
/// Отдельная функция, а не `str::parse::<i64>()`, чтобы ключ кэша и
/// `dialog_id` в API получались ОДНИМ разбором: [`dto::parse_dialog_id`]
/// отвергает мусор и значения вне диапазонов Bot API, поэтому в кэш не
/// попадёт запись, которую потом невозможно сопоставить ни с одним
/// диалогом.
pub fn dialog_key(dialog_id: &str) -> Option<i64> {
    dto::parse_dialog_id(dialog_id).and_then(|id| id.bot_api_dialog_id())
}

/// Глобальное состояние плагина.
pub struct TelegramState {
    /// Конфигурация, с которой плагин стартовал.
    ///
    /// Неизменяемая часть: пути, лимиты, `catch_up`. Актуальные креды
    /// живут в [`TelegramState::credentials`]; готовый снапшот «конфиг +
    /// свежие креды» отдаёт [`TelegramState::effective_config`].
    pub config: TelegramConfig,
    /// Креды приложения; `None` ⇒ состояние `NotConfigured`.
    pub credentials: RwLock<Option<AppCredentials>>,
    /// Broadcast всем WS-подписчикам (без фильтрации, см. шапку модуля).
    pub broadcast: broadcast::Sender<TgServerEvent>,
    /// Состояние авторизации.
    pub login: Mutex<LoginFlow>,
    /// Активное MTProto-подключение, если есть.
    pub runtime: RwLock<Option<TgRuntime>>,
    /// `upload_id` → запись незавершённой загрузки (TTL 30 минут).
    pub uploads: Mutex<HashMap<String, UploadEntry>>,
    /// Кэш резолва peer'ов: [`PeerId`] → [`PeerRef`] (см. [`crate::peers`]).
    ///
    /// Ключ — `PeerId`, а не «сырой» `i64`: он `Copy + Hash`, уже несёт в
    /// себе тип peer'а и получается прямым разбором `dialog_id` из URL
    /// ([`crate::dto::parse_dialog_id`]). С `i64`-ключом каждая запись в кэш
    /// требовала бы `bot_api_dialog_id()` → `Option`, то есть лишнюю точку
    /// отказа на горячем пути updates-воркера.
    pub peers: RwLock<HashMap<PeerId, PeerRef>>,
    /// Счётчики непрочитанного по диалогам: `dialog_id` (строкой) → сколько.
    ///
    /// Серверный кэш нужен, потому что события летят В ЛЮБУЮ вкладку и во
    /// вкладку, которая только что открылась: она получает актуальные
    /// счётчики уже в `GET /api/telegram/dialogs`, а не «с нуля». Источник
    /// истины всё равно Telegram — кэш пересевается на каждом запросе
    /// списка диалогов.
    pub unread: RwLock<HashMap<String, i32>>,
    /// Глобальные дефолты уведомлений (users / chats / broadcasts).
    ///
    /// Загружаются один раз после авторизации
    /// ([`crate::updates::refresh_notify_defaults`]) и обновляются по
    /// `updateNotifySettings` с категорийным peer'ом. Стартовое значение —
    /// «ничего не заглушено»: пока настоящие дефолты не приехали, лишний
    /// тост лучше проглоченного сообщения.
    pub notify_defaults: RwLock<NotifyDefaults>,
    /// Разрешённый `muted` по конкретным диалогам: ключ — [`dialog_key`].
    ///
    /// Значение уже учитывает наследование от глобального дефолта, поэтому
    /// потребителю (WS-кадр `new_message`, роуты) думать больше не о чем:
    /// есть запись — это ответ, нет записи — берётся дефолт категории.
    pub notify_overrides: RwLock<HashMap<i64, bool>>,
    /// Диалоги, лежащие в архиве (ключ — [`dialog_key`]).
    ///
    /// Хранится множество «в архиве», а не флаг на каждый диалог: архив —
    /// меньшинство, а знать надо ровно про него.
    pub archived_ids: RwLock<HashSet<i64>>,
    /// Папки аккаунта (dialog filters), если мы их уже спрашивали.
    ///
    /// Нужны АВТОРАЗБОРУ голосовых: белый список Telegram задаётся папками, а
    /// принадлежность чата папке из одного его id не выводится — правила
    /// папки читают и вид чата, и mute, и непрочитанное
    /// ([`dto::dialog_in_folder`]).
    ///
    /// Кэш живёт до ближайшего `updateDialogFilter*`: правки папок Telegram
    /// присылает сам, а спрашивать их на каждое входящее голосовое — лишний
    /// RPC в потоке апдейтов.
    pub folders: RwLock<Option<Vec<crate::dto::FolderDto>>>,
    /// Категория диалога (`user` / `group` / `channel`) по [`dialog_key`].
    ///
    /// Нужна ровно для выбора глобального дефолта уведомлений, когда у
    /// диалога своих настроек нет (см. блок «Зачем dialog_kinds» в шапке).
    pub dialog_kinds: RwLock<HashMap<i64, &'static str>>,
    /// Диалоги, для которых ленивый резолв флагов уже запускался.
    ///
    /// Пишется ДО запроса и не снимается при неудаче: цена промаха — лишний
    /// тост, цена повторов — RPC на каждое входящее сообщение и прямой путь
    /// к `FLOOD_WAIT` (см. [`TelegramState::claim_flags_probe`]).
    pub flags_probed: RwLock<HashSet<i64>>,
    /// Досевался ли уже архив в кэш флагов (один раз на процесс).
    pub archive_seeded: AtomicBool,
    /// Спрашивали ли мы у Telegram глобальные дефолты уведомлений.
    ///
    /// Стартовое значение [`NotifyDefaults`] («ничего не заглушено») —
    /// заглушка, а не ответ, и отличать одно от другого обязательно: всё, что
    /// ЗАПОМИНАЕТ разрешённый `muted` (ленивый резолв, досев архива), считает
    /// его с наследованием от дефолтов. Записать в кэш значение, посчитанное
    /// от заглушки, значит закрепить ошибку до конца сессии.
    pub notify_defaults_loaded: AtomicBool,
    /// Дедуп одновременных скачиваний: ключ медиа → «оповещу, когда готово».
    pub media_inflight: Mutex<HashMap<String, Arc<Notify>>>,
    /// JoinHandle'ы фоновых воркеров — abort'ятся при shutdown.
    pub workers: Mutex<Vec<JoinHandle<()>>>,
    /// Общий сигнал остановки для воркеров и WS-циклов.
    pub shutdown: CancellationToken,
    /// Просыпается при переходе в `Authorized` — воркеры, которым нужен
    /// авторизованный клиент, ждут на нём вместо polling'а.
    pub authorized_notify: Notify,
    /// Период heartbeat-пинга WS-соединения.
    ///
    /// Данные, а не константа, ровно по одной причине: поведение
    /// «нет `pong` → соединение закрывается» иначе проверялось бы тестом,
    /// который ждёт минуту. С полем интеграционный тест поднимает состояние
    /// с таймаутами в сотни миллисекунд и проверяет НАСТОЯЩИЙ цикл, а не его
    /// пересказ. В проде значения всегда равны
    /// [`crate::ws::HEARTBEAT_INTERVAL`] / [`crate::ws::IDLE_TIMEOUT`].
    pub ws_heartbeat: Duration,
    /// Сколько тишины (включая отсутствие `pong`) считается разрывом.
    pub ws_idle_timeout: Duration,

    // --- «Анализ чата через Claude» ---------------------------------------
    /// Пер-чатовые заметки, последние разборы и треды Q&A
    /// (`forge_chat_analysis::prompts`).
    ///
    /// Под `Arc` — потому что ту же мапу держит [`crate::analysis::AnalysisHub`]:
    /// сохранение разбора по концу run'а идёт под ТЕМ ЖЕ `Mutex`, что и правка
    /// заметки из хендлера, и потому не может её обогнать.
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
    /// Потолки (сколько прогонов разом и сколько ждать один) читаются из
    /// `voice.toml` ОДИН раз, в конструкторе: менять их на лету значило бы
    /// бросать уже идущие прогоны. Всё остальное — движок, язык, ключ —
    /// перечитывается на каждый запрос ([`TelegramState::voice_config`]).
    pub voice: forge_voice::VoiceHub,
    /// Ключи, которые АВТОРАЗБОР уже брал в работу (Ф4).
    ///
    /// Отдельно от дедупа реестра: тот не пускает второй прогон, пока идёт
    /// первый, а стор не даёт пересчитывать готовое, — но прогон, уже
    /// закончившийся ОТКАЗОМ, повторная доставка апдейта запускала бы снова.
    pub voice_seen: forge_voice::auto::SeenKeys,
}

impl TelegramState {
    /// Создаёт состояние из конфига. Начальное состояние логина выводится
    /// из наличия кредов (см. [`LoginFlow::from_config`]).
    pub fn new(config: TelegramConfig) -> Arc<Self> {
        Self::with_ws_timings(config, crate::ws::HEARTBEAT_INTERVAL, crate::ws::IDLE_TIMEOUT)
    }

    /// То же, что [`TelegramState::new`], но с явными таймингами WS.
    ///
    /// Нужен ИНТЕГРАЦИОННЫМ тестам: проверить закрытие по idle с боевыми
    /// 60 секундами означало бы минутный тест. Продовый путь всегда идёт
    /// через [`TelegramState::new`].
    pub fn with_ws_timings(
        config: TelegramConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
    ) -> Arc<Self> {
        let (broadcast, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let login = LoginFlow::from_config(&config);
        let credentials = match (config.api_id, config.api_hash.clone()) {
            (Some(api_id), Some(api_hash)) if !api_hash.trim().is_empty() => {
                Some(AppCredentials { api_id, api_hash })
            }
            _ => None,
        };
        // Никаких `?`: `load` не умеет возвращать ошибку по построению —
        // сломанный файл заметок не имеет права мешать плагину подняться.
        let prompts = Arc::new(Mutex::new(crate::analysis::prompts::load(
            &config.prompts_path,
        )));
        // Hub получает ТУ ЖЕ мапу и тот же путь: сохранение разбора по концу
        // run'а идёт под тем же `Mutex`, что и правка заметки из хендлера.
        // Ядро анализа общее на три мессенджера, поэтому конфиг ему передаётся
        // полями, а не типом: `TelegramConfig` крейту не виден по построению.
        let analysis = crate::analysis::AnalysisHub::new(
            config.claude_cli_path.clone(),
            config.analysis_model.clone(),
            config.analysis_timeout_secs,
        )
        .with_store(prompts.clone(), config.prompts_path.clone());
        // Потолки расшифровки — из `voice.toml`, который лежит рядом с файлом
        // сессии (в бою это `~/.config/forge/`). Битый файл не имеет права
        // мешать плагину подняться: берём встроенные умолчания, а внятный
        // отказ `voice_config` выдаст роут при первом нажатии кнопки.
        let (voice_path, meet_path) = forge_voice::config::paths_next_to(&config.session_path);
        let voice_cfg =
            forge_voice::VoiceConfig::load_from(&voice_path, &meet_path).unwrap_or_default();
        let voice = forge_voice::VoiceHub::new(
            Arc::new(crate::ws::protocol::TgVoiceSink::new(broadcast.clone())),
            voice_cfg.max_parallel,
            voice_cfg.run_timeout(),
        );
        Arc::new(Self {
            config,
            credentials: RwLock::new(credentials),
            broadcast,
            login: Mutex::new(login),
            runtime: RwLock::new(None),
            uploads: Mutex::new(HashMap::new()),
            peers: RwLock::new(HashMap::new()),
            unread: RwLock::new(HashMap::new()),
            notify_defaults: RwLock::new(NotifyDefaults::default()),
            notify_overrides: RwLock::new(HashMap::new()),
            archived_ids: RwLock::new(HashSet::new()),
            folders: RwLock::new(None),
            dialog_kinds: RwLock::new(HashMap::new()),
            flags_probed: RwLock::new(HashSet::new()),
            archive_seeded: AtomicBool::new(false),
            notify_defaults_loaded: AtomicBool::new(false),
            media_inflight: Mutex::new(HashMap::new()),
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
    /// Ищется РЯДОМ с файлом сессии — см.
    /// [`forge_voice::config::paths_next_to`]: в бою это `~/.config/forge/`, а
    /// в тестах — временный каталог, и изоляция получается без единой
    /// переменной окружения.
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
        forge_voice::config::paths_next_to(&self.config.session_path)
    }

    /// Стор готовых расшифровок рядом с медиа-кэшем плагина.
    ///
    /// Корень — тот же `cache_dir`, что у вложений: расшифровка живёт рядом с
    /// файлом, который расшифровывали, и чистка кэша по диалогу сносит их
    /// одним каталогом.
    #[must_use]
    pub fn voice_store(&self) -> forge_voice::TranscriptStore {
        forge_voice::TranscriptStore::new(&self.config.cache_dir)
    }

    /// Приёмник кадров анализа для [`crate::analysis::AnalysisHub::start`].
    ///
    /// Ядро анализа живёт в общем крейте и про [`crate::ws::protocol::TgServerMsg`]
    /// не знает: оно зовёт четыре метода трейта, а
    /// [`crate::ws::protocol::TgAnalysisSink`] перекладывает их в наши кадры и
    /// шлёт в тот же broadcast. Хелпер здесь (а не `Arc::new(...)` в каждом
    /// роуте) ради одного: приёмник обязан вещать ИМЕННО в канал этого
    /// состояния, иначе кадры run'а не увидит ни одна открытая вкладка.
    #[must_use]
    pub fn analysis_sink(&self) -> Arc<dyn crate::analysis::AnalysisSink> {
        Arc::new(crate::ws::protocol::TgAnalysisSink::new(
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

    /// Рассылает событие подписчикам. Отсутствие подписчиков — норма
    /// (никто не открыл вкладку), поэтому `SendError` глотается.
    pub fn broadcast_event(&self, ev: TgServerEvent) {
        let _ = self.broadcast.send(ev);
    }

    /// Подписка на поток событий для нового WS-соединения.
    pub fn subscribe(&self) -> broadcast::Receiver<TgServerEvent> {
        self.broadcast.subscribe()
    }

    /// Снимок состояния логина для REST/WS.
    pub async fn login_state_dto(&self) -> LoginStateDto {
        self.login.lock().await.to_dto()
    }

    /// Авторизован ли плагин.
    ///
    /// Пока MTProto не подключён, всегда `false` — на этом держатся
    /// честные 409 в REST-заглушках.
    pub async fn is_authorized(&self) -> bool {
        self.login.lock().await.is_authorized()
    }

    /// Заменяет состояние логина и рассылает `login_state` подписчикам.
    ///
    /// Единая точка перехода: broadcast делается под тем же lock'ом, что и
    /// запись, поэтому клиенты не могут увидеть события в порядке,
    /// обратном реальным переходам.
    pub async fn set_login(&self, next: LoginFlow) {
        let authorized = next.is_authorized();
        let dto = {
            let mut guard = self.login.lock().await;
            *guard = next;
            guard.to_dto()
        };
        self.broadcast_event(TgServerMsg::LoginState(dto));
        if authorized {
            self.authorized_notify.notify_waiters();
        }
    }

    /// Актуальные креды приложения (или `None`, если не настроены).
    pub async fn credentials(&self) -> Option<AppCredentials> {
        self.credentials.read().await.clone()
    }

    /// Настроены ли креды приложения прямо сейчас.
    ///
    /// Именно этот метод — источник истины для переходов автомата логина:
    /// `config.has_credentials()` знает только про старт процесса и не
    /// видит того, что пользователь ввёл в wizard'е.
    pub async fn has_credentials(&self) -> bool {
        self.credentials.read().await.is_some()
    }

    /// Запоминает новые креды (после успешного `save_credentials`).
    pub async fn set_credentials(&self, creds: AppCredentials) {
        *self.credentials.write().await = Some(creds);
    }

    /// Конфиг с подставленными актуальными кредами — то, что нужно
    /// передавать в [`crate::runtime::build_runtime`].
    pub async fn effective_config(&self) -> TelegramConfig {
        let mut cfg = self.config.clone();
        match self.credentials.read().await.clone() {
            Some(c) => {
                cfg.api_id = Some(c.api_id);
                cfg.api_hash = Some(c.api_hash);
            }
            None => {
                cfg.api_id = None;
                cfg.api_hash = None;
            }
        }
        cfg
    }

    /// Клон MTProto-клиента, если подключение живо.
    ///
    /// Клиент дешёво клонируется (Arc внутри), поэтому хендлеры берут
    /// копию и отпускают `RwLock` до того, как уйти в сеть — иначе
    /// медленный RPC блокировал бы все остальные запросы.
    pub async fn client(&self) -> Option<Client> {
        self.runtime.read().await.as_ref().map(|rt| rt.client.clone())
    }

    /// Есть ли активное MTProto-подключение.
    pub async fn has_runtime(&self) -> bool {
        self.runtime.read().await.is_some()
    }

    /// Ставит новое подключение.
    ///
    /// Вызывать ТОЛЬКО после [`Self::clear_runtime`]: `flock` на файле
    /// сессии эксклюзивен, поэтому новый рантайм невозможно построить,
    /// пока старый жив (см. [`crate::runtime::SessionLock`]).
    pub async fn set_runtime(&self, rt: TgRuntime) {
        *self.runtime.write().await = Some(rt);
    }

    /// Закрывает текущее подключение и освобождает лок сессии.
    ///
    /// Идемпотентна: без активного рантайма ничего не делает.
    pub async fn clear_runtime(&self) {
        let taken = self.runtime.write().await.take();
        if let Some(mut rt) = taken {
            rt.close().await;
        }
    }

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
    /// Сначала даёт каждому [`WORKER_STOP_GRACE`] на самостоятельный выход по
    /// `shutdown`-токену и только потом абортит. Пауза не косметическая:
    /// updates-воркер на выходе сохраняет состояние апдейтов в сессию
    /// ([`crate::updates`]), а `abort()` в произвольной await-точке этого
    /// сделать не даст — после рестарта `catch_up` пере-выкачивал бы уже
    /// показанные сообщения.
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

    /// Кладёт запись о принятом файле в реестр загрузок.
    pub async fn insert_upload(&self, upload_id: String, entry: UploadEntry) {
        self.uploads.lock().await.insert(upload_id, entry);
    }

    /// Копия записи по `upload_id` (не потребляет её).
    pub async fn upload_entry(&self, upload_id: &str) -> Option<UploadEntry> {
        self.uploads.lock().await.get(upload_id).cloned()
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
    /// Удаление staging-файлов — забота вызывающего
    /// ([`crate::routes::upload::evict_stale`]): состояние не делает
    /// файловый ввод-вывод, чтобы его можно было тестировать без ФС.
    ///
    /// `now` передаётся снаружи, а не берётся из системных часов, ровно
    /// затем, чтобы тест «через 30 минут» не спал полчаса.
    pub async fn evict_stale_uploads(&self, ttl_secs: i64, now: i64) -> Vec<UploadEntry> {
        let mut guard = self.uploads.lock().await;
        let stale: Vec<String> = guard
            .iter()
            .filter(|(_, e)| now.saturating_sub(e.created_at) > ttl_secs)
            .map(|(k, _)| k.clone())
            .collect();
        stale
            .into_iter()
            .filter_map(|k| guard.remove(&k))
            .collect()
    }

    /// Увеличивает счётчик непрочитанного диалога и возвращает новое значение.
    pub async fn bump_unread(&self, dialog_id: &str) -> i32 {
        let mut guard = self.unread.write().await;
        let slot = guard.entry(dialog_id.to_string()).or_insert(0);
        *slot = slot.saturating_add(1);
        *slot
    }

    /// Ставит счётчик непрочитанного диалога в конкретное значение
    /// (`0` ⇒ запись удаляется, чтобы кэш не рос от прочитанных диалогов).
    pub async fn set_unread(&self, dialog_id: &str, count: i32) {
        let mut guard = self.unread.write().await;
        if count > 0 {
            guard.insert(dialog_id.to_string(), count);
        } else {
            guard.remove(dialog_id);
        }
    }

    /// Текущий счётчик непрочитанного по диалогу.
    pub async fn unread_of(&self, dialog_id: &str) -> i32 {
        self.unread.read().await.get(dialog_id).copied().unwrap_or(0)
    }

    /// Заменяет кэш непрочитанного целиком (данными свежего списка диалогов).
    pub async fn reset_unread(&self, counts: HashMap<String, i32>) {
        *self.unread.write().await = counts.into_iter().filter(|(_, v)| *v > 0).collect();
    }

    // ------------------------------------------------------------------
    // Уведомления и архив (см. блок «Почему mute и архив кэшируются» в
    // шапке модуля).
    // ------------------------------------------------------------------

    /// Текущие глобальные дефолты уведомлений.
    pub async fn notify_defaults(&self) -> NotifyDefaults {
        *self.notify_defaults.read().await
    }

    /// Ставит глобальные дефолты уведомлений.
    ///
    /// Возвращает `true`, если значение действительно изменилось — по нему
    /// вызывающий решает, слать ли `notify_defaults_changed` подписчикам.
    /// Без этой проверки каждый реконнект (а он же перечитывает дефолты)
    /// рассылал бы кадр, на который фронт перерисовывает список.
    pub async fn set_notify_defaults(&self, defaults: NotifyDefaults) -> bool {
        let mut guard = self.notify_defaults.write().await;
        let changed = *guard != defaults;
        *guard = defaults;
        changed
    }

    /// Запоминает разрешённый `muted` диалога.
    pub async fn set_dialog_muted(&self, dialog_id: i64, muted: bool) {
        self.notify_overrides.write().await.insert(dialog_id, muted);
    }

    /// Разрешённый `muted` диалога, если он нам известен.
    ///
    /// `None` — это НЕ «не заглушён», а «не знаем»: вызывающий обязан взять
    /// дефолт категории ([`NotifyDefaults::for_kind`]). Ровно на этой
    /// разнице держится отсечение тостов от глобально заглушённых чатов.
    pub async fn is_dialog_muted(&self, dialog_id: i64) -> Option<bool> {
        self.notify_overrides.read().await.get(&dialog_id).copied()
    }

    /// Помечает диалог как архивный (или снимает пометку).
    pub async fn set_archived(&self, dialog_id: i64, archived: bool) {
        let mut guard = self.archived_ids.write().await;
        if archived {
            guard.insert(dialog_id);
        } else {
            guard.remove(&dialog_id);
        }
    }

    /// Лежит ли диалог в архиве. Промах кэша ⇒ `false`.
    ///
    /// В отличие от mute, здесь `Option` не нужен: незнание об архиве
    /// означает «показать», а показать лишнее безопаснее, чем спрятать.
    pub async fn is_archived(&self, dialog_id: i64) -> bool {
        self.archived_ids.read().await.contains(&dialog_id)
    }

    /// Запоминает оба флага одного диалога разом (один захват локов).
    pub async fn remember_dialog_flags(&self, dialog_id: i64, muted: bool, archived: bool) {
        self.set_dialog_muted(dialog_id, muted).await;
        self.set_archived(dialog_id, archived).await;
    }

    /// Запоминает категорию диалога (`user` / `group` / `channel`).
    ///
    /// Неизвестное значение игнорируется: в кэше категорий лучше дыра, чем
    /// строка, по которой [`NotifyDefaults::for_kind`] молча вернёт `false`.
    pub async fn set_dialog_kind(&self, dialog_id: i64, kind: &str) {
        let Some(kind) = dto::kind_static(kind) else {
            return;
        };
        self.dialog_kinds.write().await.insert(dialog_id, kind);
    }

    /// Категория диалога, если мы её видели. `None` ⇒ «не знаем».
    pub async fn dialog_kind(&self, dialog_id: i64) -> Option<&'static str> {
        self.dialog_kinds.read().await.get(&dialog_id).copied()
    }

    /// Столбит право сходить за флагами диалога в сеть.
    ///
    /// `true` ⇒ ленивый резолв запускается впервые и должен быть выполнен;
    /// `false` ⇒ по этому диалогу попытка уже была (успешная или нет), и
    /// повторять её нельзя. Запись остаётся навсегда — см. блок «Промах кэша
    /// флагов» в шапке модуля.
    pub async fn claim_flags_probe(&self, dialog_id: i64) -> bool {
        self.flags_probed.write().await.insert(dialog_id)
    }

    /// Спрашивали ли мы у Telegram глобальные дефолты уведомлений.
    ///
    /// `false` ⇒ в [`TelegramState::notify_defaults`] лежит заглушка, и
    /// запоминать посчитанный по ней `muted` нельзя (см. поле).
    pub fn notify_defaults_known(&self) -> bool {
        self.notify_defaults_loaded.load(Ordering::SeqCst)
    }

    /// Отмечает, что дефолты уведомлений реально прочитаны у Telegram.
    pub fn mark_notify_defaults_loaded(&self) {
        self.notify_defaults_loaded.store(true, Ordering::SeqCst);
    }

    /// Папки аккаунта из кэша; `None` — мы их ещё не спрашивали.
    pub async fn cached_folders(&self) -> Option<Vec<crate::dto::FolderDto>> {
        self.folders.read().await.clone()
    }

    /// Кладёт свежий список папок в кэш.
    pub async fn set_folders(&self, folders: Vec<crate::dto::FolderDto>) {
        *self.folders.write().await = Some(folders);
    }

    /// Забывает папки — их правку прислал `updateDialogFilter*`.
    pub async fn forget_folders(&self) {
        *self.folders.write().await = None;
    }

    /// Столбит право досеять архив (один раз на процесс).
    ///
    /// `true` ⇒ досев ещё не делался. Реконнект сам по себе досев не
    /// повторяет: у флапающего соединения это был бы лишний RPC каждые
    /// несколько секунд, а изменения архива приезжают апдейтом
    /// `updateFolderPeers`.
    pub fn claim_archive_seed(&self) -> bool {
        !self.archive_seeded.swap(true, Ordering::SeqCst)
    }

    /// Засевает кэши флагов свежим списком диалогов.
    ///
    /// Вызывается роутом `GET /api/telegram/dialogs`: каждый листинг —
    /// бесплатный и самый точный источник (`muted` там уже посчитан с
    /// наследованием, `archived` — по `folder_id`).
    ///
    /// Записи ДОПОЛНЯЮТСЯ, а не заменяются: страница списка — это всегда
    /// подмножество аккаунта (см. шапку модуля). Диалоги с неразбираемым
    /// `id` пропускаются молча — такой id всё равно невозможно сопоставить
    /// с апдейтом.
    pub async fn remember_dialogs(&self, dialogs: &[DialogDto]) {
        let mut muted_guard = self.notify_overrides.write().await;
        let mut archived_guard = self.archived_ids.write().await;
        let mut kinds_guard = self.dialog_kinds.write().await;
        for d in dialogs {
            let Some(key) = dialog_key(&d.id) else {
                continue;
            };
            muted_guard.insert(key, d.muted);
            if d.archived {
                archived_guard.insert(key);
            } else {
                archived_guard.remove(&key);
            }
            // Листинг — единственный источник, который отличает мегагруппу
            // от broadcast-канала: у DTO `kind` посчитан по данным чата.
            if let Some(kind) = dto::kind_static(&d.kind) {
                kinds_guard.insert(key, kind);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TelegramConfig {
        TelegramConfig::default()
    }

    #[tokio::test]
    async fn new_derives_login_from_config() {
        let st = TelegramState::new(cfg());
        assert_eq!(st.login.lock().await.kind(), "not_configured");
        assert!(!st.is_authorized().await);

        let with_creds = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            ..cfg()
        };
        let st = TelegramState::new(with_creds);
        assert_eq!(st.login.lock().await.kind(), "disconnected");
    }

    #[tokio::test]
    async fn broadcast_event_does_not_panic_without_subscribers() {
        let st = TelegramState::new(cfg());
        // Ни одного подписчика — send вернёт Err, но наружу это не течёт.
        st.broadcast_event(TgServerMsg::Ping);
        st.broadcast_event(TgServerMsg::Resync);
    }

    #[tokio::test]
    async fn subscriber_receives_broadcast() {
        let st = TelegramState::new(cfg());
        let mut rx = st.subscribe();
        st.broadcast_event(TgServerMsg::Ping);
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, TgServerMsg::Ping);
    }

    #[tokio::test]
    async fn set_login_broadcasts_login_state() {
        let st = TelegramState::new(cfg());
        let mut rx = st.subscribe();
        st.set_login(LoginFlow::Disconnected {
            error: Some("boom".into()),
        })
        .await;
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match got {
            TgServerMsg::LoginState(dto) => {
                assert_eq!(dto.state, "disconnected");
                assert_eq!(dto.error.as_deref(), Some("boom"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(st.login_state_dto().await.state, "disconnected");
    }

    #[tokio::test]
    async fn state_is_shareable_across_tasks() {
        let st = TelegramState::new(cfg());
        let clone = st.clone();
        let handle = tokio::spawn(async move { clone.is_authorized().await });
        assert!(!handle.await.unwrap());
    }

    /// Реестр загрузок: запись кладётся, читается копией, потребляется ровно
    /// один раз (это и есть защита от двойной отправки одного файла).
    #[tokio::test]
    async fn upload_registry_is_consumed_once() {
        let st = TelegramState::new(cfg());
        let entry = UploadEntry {
            path: std::path::PathBuf::from("/tmp/x.part"),
            file_name: "x.bin".into(),
            size: 10,
            mime: "application/octet-stream".into(),
            created_at: 1_800_000_000,
        };
        st.insert_upload("u1".into(), entry).await;

        assert_eq!(st.upload_entry("u1").await.unwrap().size, 10);
        // Чтение не потребляет.
        assert!(st.upload_entry("u1").await.is_some());

        assert_eq!(st.take_upload("u1").await.unwrap().file_name, "x.bin");
        assert!(st.take_upload("u1").await.is_none(), "второй раз — нечего");
        assert!(st.upload_entry("u1").await.is_none());
    }

    /// TTL считается от `created_at` по переданному «сейчас»: тест не спит.
    #[tokio::test]
    async fn evict_stale_uploads_uses_injected_now() {
        let st = TelegramState::new(cfg());
        let mk = |created_at| UploadEntry {
            path: std::path::PathBuf::from("/tmp/x.part"),
            file_name: "x".into(),
            size: 1,
            mime: "application/octet-stream".into(),
            created_at,
        };
        let now = 1_800_000_000;
        st.insert_upload("fresh".into(), mk(now)).await;
        st.insert_upload("edge".into(), mk(now - UPLOAD_TTL_SECS)).await;
        st.insert_upload("old".into(), mk(now - UPLOAD_TTL_SECS - 1)).await;

        let gone = st.evict_stale_uploads(UPLOAD_TTL_SECS, now).await;
        assert_eq!(gone.len(), 1, "ровно одна протухшая: {gone:?}");
        assert!(st.upload_entry("fresh").await.is_some());
        assert!(
            st.upload_entry("edge").await.is_some(),
            "ровно TTL — ещё живая (строгое >)"
        );
        assert!(st.upload_entry("old").await.is_none());
    }

    #[tokio::test]
    async fn shutdown_workers_is_idempotent() {
        let st = TelegramState::new(cfg());
        let token = st.shutdown.clone();
        let handle = tokio::spawn(async move { token.cancelled().await });
        st.register_worker(handle).await;

        st.shutdown.cancel();
        st.shutdown_workers().await;
        assert!(st.workers.lock().await.is_empty());
        // Повторный вызов безопасен.
        st.shutdown_workers().await;
    }

    // ------------------------------------------------------------------
    // Краевые случаи: время «наоборот», гонки, переполнения счётчиков,
    // отстающий подписчик, упрямые воркеры.
    // ------------------------------------------------------------------

    /// Часы могут прыгнуть назад (NTP-коррекция) — `created_at` из
    /// «будущего» не должен ни паниковать на вычитании, ни выбрасываться.
    /// Экстремальный `created_at` (i64::MIN — битые данные) выбрасывается
    /// без overflow-паники: на этом держится `saturating_sub`.
    #[tokio::test]
    async fn evict_survives_clock_skew_and_extreme_created_at() {
        let st = TelegramState::new(cfg());
        let mk = |created_at| UploadEntry {
            path: std::path::PathBuf::from("/tmp/x.part"),
            file_name: "x".into(),
            size: 1,
            mime: "application/octet-stream".into(),
            created_at,
        };
        let now = 1_800_000_000;
        st.insert_upload("future".into(), mk(now + 3_600)).await;
        st.insert_upload("min".into(), mk(i64::MIN)).await;

        let gone = st.evict_stale_uploads(UPLOAD_TTL_SECS, now).await;
        assert_eq!(gone.len(), 1, "ушла только запись с битым created_at");
        assert!(
            st.upload_entry("future").await.is_some(),
            "запись «из будущего» (скачок часов) не выбрасывается"
        );
        assert!(st.upload_entry("min").await.is_none());
    }

    /// Двойной клик по «Отправить»: несколько задач конкурируют за один
    /// `upload_id`. Победитель ровно один — иначе файл ушёл бы дважды.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_take_upload_has_single_winner() {
        let st = TelegramState::new(cfg());
        st.insert_upload(
            "race".into(),
            UploadEntry {
                path: std::path::PathBuf::from("/tmp/x.part"),
                file_name: "x".into(),
                size: 1,
                mime: "application/octet-stream".into(),
                created_at: 0,
            },
        )
        .await;

        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let st = st.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                st.take_upload("race").await.is_some()
            }));
        }
        let mut winners = 0;
        for t in tasks {
            if t.await.unwrap() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "запись потребляется ровно один раз");
    }

    /// Счётчик непрочитанного на пределе i32 не переполняется (заглохший
    /// канал с годами непрочитанного — реальность).
    #[tokio::test]
    async fn bump_unread_saturates_at_i32_max() {
        let st = TelegramState::new(cfg());
        st.set_unread("d1", i32::MAX).await;
        assert_eq!(st.bump_unread("d1").await, i32::MAX, "насыщение, не wrap");
        assert_eq!(st.unread_of("d1").await, i32::MAX);
    }

    /// Ноль и отрицательные значения (сервер Telegram может прислать
    /// что угодно) удаляют запись — кэш не растёт от прочитанных диалогов.
    #[tokio::test]
    async fn set_unread_non_positive_removes_entry() {
        let st = TelegramState::new(cfg());
        st.bump_unread("d1").await;
        st.set_unread("d1", 0).await;
        assert_eq!(st.unread_of("d1").await, 0);
        assert!(st.unread.read().await.is_empty(), "запись удалена, не занулена");

        st.set_unread("d2", -5).await;
        assert_eq!(st.unread_of("d2").await, 0);
        assert!(st.unread.read().await.is_empty());
    }

    /// Полная пересевка кэша фильтрует нули/отрицательные из свежего
    /// списка диалогов — они не должны занимать память.
    #[tokio::test]
    async fn reset_unread_drops_non_positive_counts() {
        let st = TelegramState::new(cfg());
        st.bump_unread("stale").await;
        let fresh: HashMap<String, i32> =
            [("a".to_string(), 2), ("b".to_string(), 0), ("c".to_string(), -1)]
                .into_iter()
                .collect();
        st.reset_unread(fresh).await;

        assert_eq!(st.unread_of("a").await, 2);
        assert_eq!(st.unread_of("b").await, 0);
        assert_eq!(st.unread_of("stale").await, 0, "старый кэш заменён целиком");
        assert_eq!(st.unread.read().await.len(), 1);
    }

    /// Отставший WS-клиент: буфер (BROADCAST_CAPACITY) переполнен —
    /// первый recv отдаёт `Lagged` (сигнал для resync), после чего канал
    /// продолжает работать, а не остаётся сломанным навсегда.
    #[tokio::test]
    async fn slow_subscriber_lags_then_resumes() {
        use tokio::sync::broadcast::error::RecvError;

        let st = TelegramState::new(cfg());
        let mut rx = st.subscribe();
        for _ in 0..(BROADCAST_CAPACITY + 2) {
            st.broadcast_event(TgServerMsg::Ping);
        }

        match rx.recv().await {
            Err(RecvError::Lagged(n)) => assert!(n >= 1, "пропущено {n}"),
            other => panic!("ожидался Lagged, получено {other:?}"),
        }
        // После Lagged канал живой: старейшее доступное событие читается.
        assert_eq!(rx.recv().await.unwrap(), TgServerMsg::Ping);
    }

    /// Воркер, игнорирующий shutdown-токен (завис в бесконечном await):
    /// `shutdown_workers` обязан его abort'нуть после WORKER_STOP_GRACE,
    /// а не зависнуть вместе с ним.
    #[tokio::test]
    async fn shutdown_aborts_worker_ignoring_the_token() {
        let st = TelegramState::new(cfg());
        st.register_worker(tokio::spawn(std::future::pending::<()>()))
            .await;

        st.shutdown.cancel();
        let done = tokio::time::timeout(
            WORKER_STOP_GRACE + std::time::Duration::from_secs(3),
            st.shutdown_workers(),
        )
        .await;
        assert!(done.is_ok(), "shutdown_workers завис на упрямом воркере");
        assert!(st.workers.lock().await.is_empty());
    }

    /// Гонка «воркер регистрируется после shutdown»: запись не теряется и
    /// подчищается следующим вызовом, а сам воркер выходит сразу — токен
    /// уже отменён.
    #[tokio::test]
    async fn worker_registered_after_shutdown_is_reaped_by_next_call() {
        let st = TelegramState::new(cfg());
        st.shutdown.cancel();
        st.shutdown_workers().await;

        let token = st.shutdown.clone();
        st.register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;
        assert_eq!(st.workers.lock().await.len(), 1);

        st.shutdown_workers().await;
        assert!(st.workers.lock().await.is_empty());
    }

    /// Конфиг с api_id, но пробельным api_hash: полукреды не должны
    /// «протекать» — ни в credentials, ни в effective_config, ни в логин.
    /// После ввода кредов в wizard'е effective_config видит их сразу.
    #[tokio::test]
    async fn whitespace_api_hash_yields_no_credentials_anywhere() {
        let half = TelegramConfig {
            api_id: Some(42),
            api_hash: Some("   ".into()),
            ..cfg()
        };
        let st = TelegramState::new(half);

        assert!(st.credentials().await.is_none());
        assert!(!st.has_credentials().await);
        assert_eq!(st.login.lock().await.kind(), "not_configured");
        let eff = st.effective_config().await;
        assert!(eff.api_id.is_none(), "api_id без hash'а не должен протекать");
        assert!(eff.api_hash.is_none());

        st.set_credentials(AppCredentials {
            api_id: 42,
            api_hash: "real".into(),
        })
        .await;
        let eff = st.effective_config().await;
        assert_eq!(eff.api_id, Some(42));
        assert_eq!(eff.api_hash.as_deref(), Some("real"));
    }

    // ------------------------------------------------------------------
    // Кэши уведомлений и архива.
    // ------------------------------------------------------------------

    /// Стартовое значение дефолтов — «ничего не заглушено», а запись
    /// сообщает, изменилось ли что-то (по этому флагу решается рассылка
    /// кадра `notify_defaults_changed`).
    #[tokio::test]
    async fn notify_defaults_report_real_changes_only() {
        let st = TelegramState::new(cfg());
        assert_eq!(st.notify_defaults().await, NotifyDefaults::default());

        let muted_users = NotifyDefaults {
            users: true,
            ..NotifyDefaults::default()
        };
        assert!(st.set_notify_defaults(muted_users).await, "первая запись");
        assert_eq!(st.notify_defaults().await, muted_users);
        assert!(
            !st.set_notify_defaults(muted_users).await,
            "повтор того же значения — не изменение"
        );
    }

    /// «Не знаем» и «не заглушён» — РАЗНЫЕ ответы: на этой разнице держится
    /// подстановка глобального дефолта категории.
    #[tokio::test]
    async fn unknown_dialog_mute_is_none_not_false() {
        let st = TelegramState::new(cfg());
        assert_eq!(st.is_dialog_muted(777_000).await, None);

        st.set_dialog_muted(777_000, false).await;
        assert_eq!(st.is_dialog_muted(777_000).await, Some(false));
        st.set_dialog_muted(777_000, true).await;
        assert_eq!(st.is_dialog_muted(777_000).await, Some(true));
    }

    /// Архив: промах кэша ⇒ «не в архиве», снятие пометки удаляет запись.
    #[tokio::test]
    async fn archived_flag_is_a_membership_set() {
        let st = TelegramState::new(cfg());
        assert!(!st.is_archived(-1_001_234_567_890).await);

        st.set_archived(-1_001_234_567_890, true).await;
        assert!(st.is_archived(-1_001_234_567_890).await);
        st.set_archived(-1_001_234_567_890, false).await;
        assert!(!st.is_archived(-1_001_234_567_890).await);
        assert!(st.archived_ids.read().await.is_empty(), "запись удалена");
    }

    /// Листинг диалогов засевает оба кэша, ДОПОЛНЯЯ их: чат из прошлой
    /// страницы (архив, папка) не должен исчезать из знания сервера.
    #[tokio::test]
    async fn remember_dialogs_merges_and_skips_broken_ids() {
        let st = TelegramState::new(cfg());
        st.remember_dialog_flags(-100, true, true).await;

        let page = vec![
            DialogDto {
                id: "777000".into(),
                muted: true,
                archived: false,
                ..DialogDto::default()
            },
            DialogDto {
                id: "-1001234567890".into(),
                muted: false,
                archived: true,
                ..DialogDto::default()
            },
            // Битый id (пустая строка приходит, например, от удаления в ЛС).
            DialogDto {
                id: String::new(),
                muted: true,
                archived: true,
                ..DialogDto::default()
            },
        ];
        st.remember_dialogs(&page).await;

        assert_eq!(st.is_dialog_muted(777_000).await, Some(true));
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(false));
        assert!(st.is_archived(-1_001_234_567_890).await);
        assert!(!st.is_archived(777_000).await);
        // Запись из прошлой страницы уцелела.
        assert_eq!(st.is_dialog_muted(-100).await, Some(true));
        assert!(st.is_archived(-100).await);
        assert_eq!(
            st.notify_overrides.read().await.len(),
            3,
            "битый id в кэш не попал"
        );

        // Повторный листинг с изменившимися флагами перезаписывает записи.
        st.remember_dialogs(&[DialogDto {
            id: "-1001234567890".into(),
            muted: true,
            archived: false,
            ..DialogDto::default()
        }])
        .await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(true));
        assert!(!st.is_archived(-1_001_234_567_890).await);
    }

    /// Ключ кэша — тот же формат Bot API, что и `dialog_id` в JSON.
    #[test]
    fn dialog_key_matches_the_public_dialog_id() {
        assert_eq!(dialog_key("777000"), Some(777_000));
        assert_eq!(dialog_key("-1001234567890"), Some(-1_001_234_567_890));
        for raw in ["", "  ", "abc", "0", "1e10"] {
            assert!(dialog_key(raw).is_none(), "{raw:?} не должен давать ключ");
        }
    }

    /// Границы диапазонов Bot API: ключ кэша обязан приниматься ровно там,
    /// где Telegram считает id валидным, — и ни шагом дальше.
    ///
    /// Иначе получается худший из вариантов: `remember_dialogs` молча
    /// пропускает диалог (его id «не разобрался»), кэш о нём ничего не знает,
    /// и WS-кадр про него подставляет дефолт категории вместо правды.
    #[test]
    fn dialog_key_honours_every_bot_api_boundary() {
        // (значение, ожидание, за что отвечает)
        let cases: [(i64, bool, &str); 12] = [
            (1, true, "первый пользователь"),
            (0xff_ffff_ffff, true, "последний пользователь"),
            (0x100_0000_0000, false, "на единицу дальше пользователей"),
            (-1, true, "первая малая группа"),
            (-999_999_999_999, true, "последняя малая группа"),
            (-1_000_000_000_000, false, "дыра между группами и каналами"),
            (-1_000_000_000_001, true, "первый канал"),
            (-1_997_852_516_352, true, "последний канал"),
            (-1_997_852_516_353, false, "за границей каналов"),
            (-2_002_147_483_649, true, "первый monoforum"),
            (-4_000_000_000_000, true, "последний monoforum"),
            (-4_000_000_000_001, false, "за границей monoforum"),
        ];
        for (value, ok, label) in cases {
            let key = dialog_key(&value.to_string());
            assert_eq!(key.is_some(), ok, "{label} ({value})");
            if ok {
                assert_eq!(key, Some(value), "{label}: ключ обязан совпасть с id");
                // И обратный путь: то, что попало в кэш, обязано находиться
                // по тому же dialog_id, который уходит во фронт.
                let id = dto::parse_dialog_id(&value.to_string()).unwrap();
                assert_eq!(dto::dialog_id_string(id), value.to_string(), "{label}");
            }
        }
    }

    /// Гонка «апдейт против листинга»: итог не имеет права зависеть от того,
    /// что пришло раньше — побеждает ПОСЛЕДНЕЕ известие, а не источник.
    ///
    /// Оба порядка реальны: WS поднимается раньше первого `GET /dialogs`
    /// (апдейт первый), а mute с телефона во время просмотра списка даёт
    /// обратную последовательность.
    #[tokio::test]
    async fn per_dialog_flags_converge_regardless_of_arrival_order() {
        let page = |muted, archived| {
            vec![DialogDto {
                id: "-1001234567890".into(),
                muted,
                archived,
                ..DialogDto::default()
            }]
        };

        // Апдейт → листинг: побеждает листинг.
        let st = TelegramState::new(cfg());
        st.set_dialog_muted(-1_001_234_567_890, true).await;
        st.remember_dialogs(&page(false, true)).await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(false));
        assert!(st.is_archived(-1_001_234_567_890).await);

        // Листинг → апдейт: побеждает апдейт, но ТОЛЬКО в своей части —
        // архив он не трогает (updateNotifySettings про папки не знает).
        let st = TelegramState::new(cfg());
        st.remember_dialogs(&page(false, true)).await;
        st.set_dialog_muted(-1_001_234_567_890, true).await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(true));
        assert!(
            st.is_archived(-1_001_234_567_890).await,
            "mute и архив — независимые оси"
        );
    }

    /// Смапленная как диалог строка «Архив» СТИРАЕТ правду об архивном чате.
    ///
    /// Ровно из-за этого листинги отбрасывают `dialogFolder` ещё до сборки
    /// DTO ([`crate::dto::is_folder_row`]). Тест держит цену ошибки на виду:
    /// достаточно одной такой записи, чтобы заглушённый архивный чат снова
    /// начал «звенеть» — то есть чтобы вернулась исходная жалоба.
    #[tokio::test]
    async fn a_folder_row_would_erase_the_archived_chat_it_points_at() {
        let st = TelegramState::new(cfg());
        // Листинг архива рассказал правду про чат.
        st.remember_dialogs(&[DialogDto {
            id: "-1001234567890".into(),
            muted: true,
            archived: true,
            ..DialogDto::default()
        }])
        .await;
        assert_eq!(st.is_dialog_muted(-1_001_234_567_890).await, Some(true));

        // А теперь — DTO, каким его сделала бы строка «Архив» основного
        // списка: тот же id, но обнулённые флаги (у `dialogFolder` нет ни
        // `notify_settings`, ни `folder_id`).
        st.remember_dialogs(&[DialogDto {
            id: "-1001234567890".into(),
            muted: false,
            archived: false,
            ..DialogDto::default()
        }])
        .await;
        assert_eq!(
            st.is_dialog_muted(-1_001_234_567_890).await,
            Some(false),
            "вот она, подмена: чат снова «звенит»"
        );
        assert!(!st.is_archived(-1_001_234_567_890).await);
    }

    /// Кэши флагов не растут от повторов и не путают соседей: тысяча
    /// диалогов, перезаписанных дважды, даёт ровно тысячу записей.
    ///
    /// Проверка не про производительность, а про ключ: ошибка в `dialog_key`
    /// (например, разбор с потерей знака) проявилась бы именно раздвоением
    /// записей, а не падением.
    #[tokio::test]
    async fn flag_caches_are_keyed_not_appended() {
        let st = TelegramState::new(cfg());
        let page: Vec<DialogDto> = (1..=1000_i64)
            .map(|i| DialogDto {
                id: (-i).to_string(), // малые группы: -1..-1000
                muted: i % 2 == 0,
                archived: i % 3 == 0,
                ..DialogDto::default()
            })
            .collect();
        st.remember_dialogs(&page).await;
        st.remember_dialogs(&page).await;

        assert_eq!(st.notify_overrides.read().await.len(), 1000);
        assert_eq!(st.archived_ids.read().await.len(), 333);
        assert_eq!(st.is_dialog_muted(-2).await, Some(true));
        assert_eq!(st.is_dialog_muted(-3).await, Some(false));
        assert!(st.is_archived(-3).await);
        assert!(!st.is_archived(-2).await);

        // Повторный листинг с ПЕРЕВЁРНУТЫМИ флагами перезаписывает всё, а не
        // добавляет вторую запись рядом.
        let flipped: Vec<DialogDto> = page
            .iter()
            .map(|d| DialogDto {
                muted: !d.muted,
                archived: !d.archived,
                ..d.clone()
            })
            .collect();
        st.remember_dialogs(&flipped).await;
        assert_eq!(st.notify_overrides.read().await.len(), 1000);
        assert_eq!(st.archived_ids.read().await.len(), 667);
        assert_eq!(st.is_dialog_muted(-2).await, Some(false));
        assert!(st.is_archived(-2).await);
    }
}
