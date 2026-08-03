//! Разделяемое состояние плагина VK.
//!
//! [`VkState`] создаётся один раз в [`crate::init`], оборачивается в `Arc` и
//! раздаётся axum-хендлерам через `with_state`, WS-соединениям и фоновым
//! воркерам. Внутри — только примитивы синхронизации tokio, поэтому
//! структура `Send + Sync` и переживает любое количество клонов `Arc`.
//!
//! ## Почему такой набор полей
//!
//! - `broadcast` (ёмкость 1024) — единственный путь событий к WS-клиентам.
//!   Фильтрации по активному диалогу НЕТ: unread-бейдж и тосты должны
//!   работать с любой вкладки, поэтому сервер вещает всё, а решает фронт.
//! - `login` под `Mutex` — переходы автомата строго сериализованы.
//! - `token` под `RwLock` — ЕДИНСТВЕННАЯ часть конфигурации, которая меняется
//!   во время работы процесса (пользователь вставляет токен в UI). Читается
//!   на каждом вызове API, пишется при логине/логауте.
//! - `uploads` — реестр незавершённых загрузок; запись живёт до
//!   [`UPLOAD_TTL_SECS`], после чего её подчищает воркер (иначе оборванная
//!   вкладка навсегда оставляла бы staging-файл).
//! - `lp_cursor` — курсор LongPoll (`server`/`key`/`ts`/`pts`).
//! - `health` — здоровье последнего сетевого вызова, см. блок ниже.
//! - `shutdown` + `workers` — graceful shutdown по образцу forge-echo.
//!
//! ## Чего здесь НЕТ по сравнению с Telegram
//!
//! Ни `runtime`, ни `SessionLock`, ни кэша `PeerRef`: у VK нет постоянного
//! соединения и нет MTProto-сессии. Единственный артефакт учётки — строка
//! токена, и она живёт в [`crate::config`].
//!
//! Поле `peers` — НЕ телеграмный кэш `PeerRef`: адресация у VK тривиальна
//! (`peer_id` самодостаточен), и кэшируются там только ИМЕНА и присутствие,
//! чтобы не тратить на них лимит 3 rps (см. [`crate::peers`]).
//!
//! ## Что такое 503 `not_connected` без постоянного соединения
//!
//! У Telegram это «нет MTProto-сокета» — состояние, которое можно потрогать.
//! У VK соединения нет вовсе, а трёхступенчатая лестница проверок во всех
//! data-роутах (авторизация 409 → соединение 503 → резолв peer 404) обязана
//! остаться идентичной, иначе фронт-модули перестают быть близнецами.
//!
//! Смысл 503 наполняет [`ConnHealth`]: каждый успешный вызов ставит `Ok`,
//! каждый транспортный отказ — `Down{since, reason}`. [`VkState::connected`]
//! возвращает `false`, только пока здоровье `Down` И с момента отказа прошло
//! меньше [`HEALTH_GRACE_SECS`]. Грейс обязателен в обе стороны: без него
//! один сетевой чих залипал бы навсегда (никто не «чинит» здоровье, если все
//! запросы отвергаются на входе), а без окна вовсе 503 никогда бы не
//! возникал и лестница проверок стала бы фикцией.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::api::VkClient;
use crate::config::{SecretToken, VkConfig};
use crate::login::{LoginFlow, LoginStateDto};
use crate::ws::protocol::{VkServerEvent, VkServerMsg};

/// Ёмкость broadcast-канала. 1024 кадра — запас на всплеск событий (пачка
/// LongPoll в активной беседе, догон после реконнекта) без отставания
/// нормального клиента. Отставший получает `Lagged` ⇒ WS шлёт `resync`.
pub const BROADCAST_CAPACITY: usize = 1024;

/// Сколько живёт запись незавершённой загрузки, секунды.
pub const UPLOAD_TTL_SECS: i64 = 30 * 60;

/// Сколько ждать самостоятельного выхода воркера при shutdown, прежде чем
/// оборвать его `abort()` (см. [`VkState::shutdown_workers`]).
pub const WORKER_STOP_GRACE: Duration = Duration::from_secs(2);

/// Сколько адресов картинок captcha помним одновременно.
///
/// Captcha живёт секунды и всегда одна; запас в 16 записей нужен только на
/// случай нескольких вкладок и параллельных запросов. Карта ограничена
/// намеренно: без потолка она была бы неограниченно растущей структурой,
/// наполняемой из ответов VK.
pub const CAPTCHA_MEMO_CAPACITY: usize = 16;

/// Окно, в течение которого транспортный отказ трактуется как «связи нет».
///
/// 3 секунды — компромисс: короче, чем время реакции пользователя на 503, и
/// длиннее, чем один сетевой чих. По истечении грейса запросы снова
/// пропускаются, и первый же успешный вызов вернёт здоровье в `Ok`.
pub const HEALTH_GRACE_SECS: i64 = 3;

/// Сколько живёт запись короткоживущего кэша сообщений, секунды.
///
/// Минуты хватает, чтобы браузер выгреб все карточки одного сообщения
/// (десять запросов подряд при бюджете 3 rps — это около трёх секунд), и мало,
/// чтобы кэш успел стать «второй лентой»: он держит вложения ради адреса
/// скачивания, а адреса VK подписаны и протухают.
pub const MESSAGE_CACHE_TTL_SECS: i64 = 60;

/// Сколько сообщений помним одновременно.
///
/// Потолок обязателен: карта наполняется по запросам браузера, и без него
/// открытая на сутки вкладка с длинной перепиской росла бы неограниченно.
pub const MESSAGE_CACHE_CAPACITY: usize = 256;

/// Здоровье последнего обращения к api.vk.com.
///
/// Заменяет собой телеграмное «есть ли сокет» — см. блок в шапке модуля.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnHealth {
    /// Последний вызов прошёл. `at` — unix-время успеха.
    Ok {
        /// Unix-время последнего успешного вызова.
        at: i64,
    },
    /// Последний вызов упал по транспорту. `since` — unix-время отказа.
    Down {
        /// Unix-время отказа.
        since: i64,
        /// Человекочитаемая причина (уходит в `message` ошибки 503).
        reason: String,
    },
}

impl Default for ConnHealth {
    /// Стартовое значение — `Ok(at: 0)`: до первого вызова у нас нет причин
    /// считать сеть сломанной, а «неизвестно» отдельным состоянием было бы
    /// третьей веткой без единого потребителя.
    fn default() -> Self {
        Self::Ok { at: 0 }
    }
}

/// Курсор LongPoll: адрес сервера, ключ и позиция.
///
/// Персистится в `<cache_dir>/longpoll.json`, наполняется в Ф5. Поле в
/// состоянии заводится здесь, чтобы `state.rs` не пришлось править в фазе,
/// где меняется только логика воркера.
///
/// `pts` приходит только при `getLongPollServer(need_pts=1)` и бите 32 в
/// `mode` — без него `messages.getLongPollHistory` (обогащение пачки одним
/// запросом) вызвать нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpCursor {
    /// Хост LongPoll-сервера, выданный VK.
    pub server: String,
    /// Ключ доступа к нему (протухает — это `failed:2`).
    pub key: String,
    /// Позиция во времени событий.
    pub ts: u64,
    /// Позиция в истории сообщений (для `getLongPollHistory`).
    pub pts: Option<u64>,
}

/// Запись реестра незавершённых загрузок.
///
/// Заполняется при `POST /api/vk/upload` (браузер → staging-файл на нашем
/// диске) и потребляется при отправке сообщения с `upload_id`.
///
/// Хранится ПУТЬ, а не готовая attachment-строка VK, и это не «как у
/// Telegram», а вынужденно: upload-сервер VK выдаётся под КОНКРЕТНЫЙ
/// `peer_id`, а на шаге `/upload` адресат ещё неизвестен (пользователь может
/// прикрепить файл и передумать, куда слать). Поэтому весь 3-шаговый
/// конвейер VK живёт внутри `POST /messages`.
#[derive(Debug, Clone)]
pub struct UploadEntry {
    /// Путь staging-файла на диске.
    pub path: std::path::PathBuf,
    /// Исходное имя файла.
    pub file_name: String,
    /// Размер в байтах.
    pub size: u64,
    /// MIME по расширению имени — по нему выбирается ветка конвейера
    /// (photo / doc / audio_message).
    pub mime: String,
    /// Unix-время создания записи — база для [`UPLOAD_TTL_SECS`].
    pub created_at: i64,
}

/// Глобальное состояние плагина.
pub struct VkState {
    /// Конфигурация, с которой плагин стартовал (неизменяемая часть).
    ///
    /// Актуальный токен живёт в [`VkState::token`]; готовый снапшот «конфиг +
    /// свежий токен» отдаёт [`VkState::effective_config`].
    pub config: VkConfig,
    /// Актуальный access-токен; пустой ⇒ состояние `not_configured`.
    pub token: RwLock<SecretToken>,
    /// Broadcast всем WS-подписчикам (без фильтрации, см. шапку модуля).
    pub broadcast: broadcast::Sender<VkServerEvent>,
    /// Состояние авторизации.
    pub login: Mutex<LoginFlow>,
    /// `upload_id` → запись незавершённой загрузки (TTL 30 минут).
    pub uploads: Mutex<HashMap<String, UploadEntry>>,
    /// Курсор LongPoll (Ф5).
    pub lp_cursor: RwLock<Option<LpCursor>>,
    /// Здоровье последнего сетевого вызова — источник смысла для 503.
    ///
    /// Под `Arc` намеренно: ту же ячейку держит [`crate::api::VkClient`]
    /// ([`VkState::health_handle`]). Собственная копия у клиента означала бы,
    /// что data-роуты отдают 503 по данным, ничего не знающим о реальных
    /// вызовах, — то есть лестница проверок стала бы фикцией.
    pub health: Arc<RwLock<ConnHealth>>,
    /// JoinHandle'ы фоновых воркеров — abort'ятся при shutdown.
    pub workers: Mutex<Vec<JoinHandle<()>>>,
    /// Общий сигнал остановки для воркеров и WS-циклов.
    pub shutdown: CancellationToken,
    /// Просыпается при переходе в `Authorized` — LongPoll-воркер ждёт на нём
    /// вместо polling'а, поэтому вставка токена в работающем процессе
    /// поднимает realtime без рестарта devforge.
    pub authorized_notify: Notify,
    /// `captcha_sid` → URL картинки у VK.
    ///
    /// Наполняется ТОЛЬКО из ответов VK ([`crate::login::on_captcha`] и
    /// маппинг ошибок в роутах) и читается прокси `GET /api/vk/captcha/:sid`.
    /// Это принципиально: браузеру нельзя дать передать нам произвольный URL
    /// для скачивания (открытый прокси = SSRF), а `sid` сам по себе адреса не
    /// содержит. Ёмкость ограничена [`CAPTCHA_MEMO_CAPACITY`].
    pub captcha_imgs: Mutex<Vec<(String, String)>>,
    /// Кэш клиента API: ОДИН клиент на токен.
    ///
    /// Вместе с клиентом создаётся троттлер, поэтому два клиента означали бы
    /// двойной бюджет запросов к VK (лимит 3 rps выдаётся на токен, а не на
    /// соединение). Пара хранится вместе с токеном, под который собрана:
    /// смена токена в UI обязана привести к пересборке, иначе после логина мы
    /// продолжили бы ходить со старым секретом.
    pub client: RwLock<Option<(SecretToken, Arc<VkClient>)>>,
    /// Кэш имён и присутствия собеседников.
    ///
    /// Не под `Mutex`: [`crate::peers::PeerCache`] сам синхронизирован внутри
    /// (`std::sync::RwLock`) и не делает `.await` — иначе резолв имени
    /// оказался бы точкой отмены на каждой строке ленты.
    pub peers: crate::peers::PeerCache,
    /// Период heartbeat-пинга WS-соединения.
    ///
    /// Данные, а не константа, ровно по одной причине: поведение «нет `pong`
    /// → соединение закрывается» иначе проверялось бы тестом, который ждёт
    /// минуту. С полем интеграционный тест поднимает состояние с таймаутами в
    /// сотни миллисекунд и проверяет НАСТОЯЩИЙ цикл, а не его пересказ.
    pub ws_heartbeat: Duration,
    /// Сколько тишины (включая отсутствие `pong`) считается разрывом.
    pub ws_idle_timeout: Duration,
    /// Тайминги LongPoll-воркера.
    ///
    /// Данные, а не константы, по той же причине, что и тайминги WS: иначе
    /// тест «после `failed:2` воркер не потерял ни одного кадра» обязан был бы
    /// спать боевую секунду backoff'а, а тест «вставка токена поднимает
    /// воркера» — две секунды тика [`crate::longpoll::AUTH_POLL`].
    pub lp: crate::longpoll::LpTimings,
    /// Дедуп скачиваний ПО КЛЮЧУ КЭША
    /// ([`crate::media_cache::CacheKey::id`] → будильник ожидающих).
    ///
    /// Копия телеграмного реестра: несколько вкладок, открывших одну и ту же
    /// картинку, обязаны стоить одного скачивания. Владелец ключа снимает
    /// запись через guard (в том числе при панике), ожидающие перечитывают
    /// кэш по `Notify`.
    pub media_inflight: Mutex<HashMap<String, Arc<Notify>>>,
    /// Дедуп резолва СООБЩЕНИЯ (`(peer_id, message_id)` → будильник).
    ///
    /// ДОБАВЛЕНИЕ ПРОТИВ TELEGRAM, продиктованное лимитом 3 rps. Адрес
    /// вложения добывается через `messages.getById`, который отдаёт ВСЁ
    /// сообщение целиком, — а карточек у галереи из десяти фото десять, и
    /// каждая просит свой индекс. Без этого уровня галерея стоила бы десяти
    /// одинаковых `getById`, то есть трёх секунд троттлинга на одну картинку.
    pub message_inflight: Mutex<HashMap<(i64, i32), Arc<Notify>>>,
    /// Короткоживущий кэш разобранных сообщений для резолва вложений.
    ///
    /// Живёт [`MESSAGE_CACHE_TTL_SECS`] и не больше [`MESSAGE_CACHE_CAPACITY`]
    /// записей. Это НЕ кэш ленты и не замена `messages.getById`: он существует
    /// ровно на время, пока браузер выгребает карточки одного сообщения. TTL
    /// короткий намеренно — URL внутри вложений подписанные и протухающие, и
    /// держать их дольше минуты означало бы кэшировать ссылку, чего плагин не
    /// делает НИКОГДА (см. шапку [`crate::media_cache`]).
    pub message_cache: Mutex<Vec<CachedMessage>>,
}

/// Запись короткоживущего кэша сообщений (см. [`VkState::message_cache`]).
#[derive(Debug, Clone)]
pub struct CachedMessage {
    /// Беседа.
    pub peer_id: i64,
    /// Глобальный идентификатор сообщения.
    pub message_id: i32,
    /// Вложения сообщения в сыром виде — из них берётся адрес скачивания.
    pub attachments: Arc<Vec<crate::api::schema::AttachmentRaw>>,
    /// Unix-время записи — база для [`MESSAGE_CACHE_TTL_SECS`].
    pub cached_at: i64,
}

impl VkState {
    /// Создаёт состояние из конфига. Начальное состояние логина выводится из
    /// наличия токена (см. [`LoginFlow::from_config`]).
    pub fn new(config: VkConfig) -> Arc<Self> {
        Self::with_ws_timings(config, crate::ws::HEARTBEAT_INTERVAL, crate::ws::IDLE_TIMEOUT)
    }

    /// То же, что [`VkState::new`], но с явными таймингами WS.
    ///
    /// Нужен ИНТЕГРАЦИОННЫМ тестам: проверить закрытие по idle с боевыми 60
    /// секундами означало бы минутный тест. Продовый путь всегда идёт через
    /// [`VkState::new`].
    pub fn with_ws_timings(
        config: VkConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
    ) -> Arc<Self> {
        let lp = crate::longpoll::LpTimings::from_config(&config);
        Self::with_timings(config, ws_heartbeat, ws_idle_timeout, lp)
    }

    /// То же, но с явными таймингами И WS, И LongPoll-воркера.
    ///
    /// Нужен интеграционному стенду `tests/vk_stub.rs`: без него сценарий
    /// «`failed:2` → новый ключ → снова `updates`» пришлось бы гонять с боевой
    /// лестницей backoff'а, то есть минутами.
    pub fn with_timings(
        config: VkConfig,
        ws_heartbeat: Duration,
        ws_idle_timeout: Duration,
        lp: crate::longpoll::LpTimings,
    ) -> Arc<Self> {
        let (broadcast, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let login = LoginFlow::from_config(&config);
        let token = config.access_token.clone();
        Arc::new(Self {
            config,
            token: RwLock::new(token),
            broadcast,
            login: Mutex::new(login),
            uploads: Mutex::new(HashMap::new()),
            lp_cursor: RwLock::new(None),
            health: Arc::new(RwLock::new(ConnHealth::default())),
            workers: Mutex::new(Vec::new()),
            shutdown: CancellationToken::new(),
            authorized_notify: Notify::new(),
            captcha_imgs: Mutex::new(Vec::new()),
            client: RwLock::new(None),
            peers: crate::peers::PeerCache::new(),
            ws_heartbeat,
            ws_idle_timeout,
            lp,
            media_inflight: Mutex::new(HashMap::new()),
            message_inflight: Mutex::new(HashMap::new()),
            message_cache: Mutex::new(Vec::new()),
        })
    }

    /// Рассылает событие подписчикам. Отсутствие подписчиков — норма (никто
    /// не открыл вкладку), поэтому `SendError` глотается.
    pub fn broadcast_event(&self, ev: VkServerEvent) {
        let _ = self.broadcast.send(ev);
    }

    /// Подписка на поток событий для нового WS-соединения.
    pub fn subscribe(&self) -> broadcast::Receiver<VkServerEvent> {
        self.broadcast.subscribe()
    }

    /// Снимок состояния логина для REST/WS.
    pub async fn login_state_dto(&self) -> LoginStateDto {
        self.login.lock().await.to_dto()
    }

    /// Авторизован ли плагин.
    pub async fn is_authorized(&self) -> bool {
        self.login.lock().await.is_authorized()
    }

    /// Заменяет состояние логина и рассылает `login_state` подписчикам.
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
        self.broadcast_event(VkServerMsg::LoginState(dto));
        if authorized {
            self.authorized_notify.notify_waiters();
        }
    }

    /// Актуальный токен.
    pub async fn token(&self) -> SecretToken {
        self.token.read().await.clone()
    }

    /// Настроен ли токен прямо сейчас.
    ///
    /// Именно этот метод — источник истины для переходов автомата логина:
    /// `config.has_token()` знает только про старт процесса и не видит того,
    /// что пользователь вставил в UI.
    pub async fn has_token(&self) -> bool {
        !self.token.read().await.is_empty()
    }

    /// Запоминает новый токен (после успешного `save_token`).
    ///
    /// Кэш клиента сбрасывается ЗДЕСЬ ЖЕ: клиент держит копию секрета внутри,
    /// и оставленный после логина старый экземпляр продолжил бы ходить в VK с
    /// прошлым токеном (после logout — с уже стёртым).
    pub async fn set_token(&self, token: SecretToken) {
        *self.token.write().await = token;
        *self.client.write().await = None;
    }

    /// Клиент API для текущего токена.
    ///
    /// Пересобирается только при смене токена; во всех остальных случаях
    /// отдаётся тот же `Arc`, а значит и тот же троттлер. Именно поэтому у
    /// плагина не может незаметно появиться второй бюджет запросов.
    ///
    /// # Errors
    ///
    /// Только невозможность собрать `reqwest::Client` (нерабочий TLS-бэкенд,
    /// исчерпание дескрипторов).
    pub async fn api_client(&self) -> anyhow::Result<Arc<VkClient>> {
        let cfg = self.effective_config().await;
        {
            let guard = self.client.read().await;
            if let Some((token, client)) = guard.as_ref() {
                if *token == cfg.access_token {
                    return Ok(client.clone());
                }
            }
        }
        let mut guard = self.client.write().await;
        // Повторная проверка под write-lock: пока мы ждали, соседняя задача
        // могла собрать клиент — второй экземпляр означал бы второй троттлер.
        if let Some((token, client)) = guard.as_ref() {
            if *token == cfg.access_token {
                return Ok(client.clone());
            }
        }
        let client = Arc::new(VkClient::new(&cfg, self.health_handle())?);
        *guard = Some((cfg.access_token.clone(), client.clone()));
        Ok(client)
    }

    // ------------------------------------------------------------------
    // Captcha.
    // ------------------------------------------------------------------

    /// Запоминает адрес картинки captcha под её `sid`.
    ///
    /// Повторный `sid` обновляет адрес, а не плодит запись: VK иногда
    /// присылает тот же `sid` с новой подписью в URL.
    pub async fn remember_captcha(&self, sid: &str, img: &str) {
        if sid.is_empty() || img.is_empty() {
            return;
        }
        let mut guard = self.captcha_imgs.lock().await;
        if let Some(slot) = guard.iter_mut().find(|(k, _)| k == sid) {
            slot.1 = img.to_string();
            return;
        }
        if guard.len() >= CAPTCHA_MEMO_CAPACITY {
            // Вытесняем самую старую: FIFO достаточно, картинка живёт секунды.
            guard.remove(0);
        }
        guard.push((sid.to_string(), img.to_string()));
    }

    /// Забывает все адреса картинок (logout).
    ///
    /// Captcha принадлежит КОНКРЕТНОЙ сессии: после отвязки токена её `sid`
    /// уже не примут, а сама картинка — след прошлого аккаунта.
    pub async fn forget_captchas(&self) {
        self.captcha_imgs.lock().await.clear();
    }

    /// Адрес картинки по `sid`, если он ещё помнится.
    pub async fn captcha_img(&self, sid: &str) -> Option<String> {
        self.captcha_imgs
            .lock()
            .await
            .iter()
            .find(|(k, _)| k == sid)
            .map(|(_, v)| v.clone())
    }

    /// Конфиг с подставленным актуальным токеном — то, что нужно передавать в
    /// конструктор клиента API (Ф2).
    pub async fn effective_config(&self) -> VkConfig {
        let mut cfg = self.config.clone();
        cfg.access_token = self.token.read().await.clone();
        cfg
    }

    // ------------------------------------------------------------------
    // Здоровье соединения.
    // ------------------------------------------------------------------

    /// Текущее здоровье.
    pub async fn health(&self) -> ConnHealth {
        self.health.read().await.clone()
    }

    /// Ячейка здоровья для [`crate::api::VkClient::new`].
    ///
    /// Клиент отмечает успехи и транспортные отказы ПРЯМО в это состояние,
    /// поэтому `503 not_connected` в data-роутах опирается на факты, а не на
    /// отдельную копию, которую пришлось бы синхронизировать вручную.
    pub fn health_handle(&self) -> Arc<RwLock<ConnHealth>> {
        self.health.clone()
    }

    /// Отмечает успешный сетевой вызов.
    pub async fn note_ok(&self, now: i64) {
        *self.health.write().await = ConnHealth::Ok { at: now };
    }

    /// Отмечает транспортный отказ.
    ///
    /// Повторный отказ НЕ сдвигает `since`: иначе бесконечная череда сбоев
    /// продлевала бы окно 503 навсегда и запросы перестали бы доходить до
    /// сети — то есть здоровье уже никогда не смогло бы починиться.
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

    /// Пропускать ли запрос дальше в сеть.
    ///
    /// `false` только внутри окна [`HEALTH_GRACE_SECS`] после отказа — по
    /// этому ответу data-роуты отдают `503 not_connected`.
    pub async fn connected(&self, now: i64) -> bool {
        match &*self.health.read().await {
            ConnHealth::Ok { .. } => true,
            ConnHealth::Down { since, .. } => now.saturating_sub(*since) >= HEALTH_GRACE_SECS,
        }
    }

    /// Причина последнего отказа, если она ещё актуальна (для текста 503).
    pub async fn down_reason(&self, now: i64) -> Option<String> {
        match &*self.health.read().await {
            ConnHealth::Down { since, reason } if now.saturating_sub(*since) < HEALTH_GRACE_SECS => {
                Some(reason.clone())
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Курсор LongPoll.
    // ------------------------------------------------------------------

    /// Текущий курсор LongPoll.
    pub async fn lp_cursor(&self) -> Option<LpCursor> {
        self.lp_cursor.read().await.clone()
    }

    /// Ставит курсор LongPoll.
    pub async fn set_lp_cursor(&self, cursor: Option<LpCursor>) {
        *self.lp_cursor.write().await = cursor;
    }

    // ------------------------------------------------------------------
    // Воркеры.
    // ------------------------------------------------------------------

    /// Регистрирует фонового воркера для последующего graceful shutdown.
    pub async fn register_worker(&self, handle: JoinHandle<()>) {
        self.workers.lock().await.push(handle);
    }

    /// Останавливает всех зарегистрированных воркеров.
    ///
    /// Сначала даёт каждому [`WORKER_STOP_GRACE`] на самостоятельный выход по
    /// `shutdown`-токену и только потом абортит. Пауза не косметическая:
    /// LongPoll-воркер на выходе персистит курсор, а `abort()` в произвольной
    /// await-точке этого сделать не даст — после рестарта поток событий
    /// начался бы «с сейчас» с потерей накопленного.
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
    // Реестр загрузок.
    // ------------------------------------------------------------------

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
    /// Удаление staging-файлов — забота вызывающего: состояние не делает
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
        stale.into_iter().filter_map(|k| guard.remove(&k)).collect()
    }

    // -----------------------------------------------------------------
    // Короткоживущий кэш сообщений (резолв адресов вложений, Ф7).
    // -----------------------------------------------------------------

    /// Вложения сообщения, если они разбирались меньше
    /// [`MESSAGE_CACHE_TTL_SECS`] секунд назад.
    ///
    /// `now` приходит снаружи по той же причине, что и у
    /// [`VkState::evict_stale_uploads`]: тест протухания не должен ждать минуту.
    pub async fn cached_message(
        &self,
        peer_id: i64,
        message_id: i32,
        now: i64,
    ) -> Option<Arc<Vec<crate::api::schema::AttachmentRaw>>> {
        let guard = self.message_cache.lock().await;
        guard
            .iter()
            .find(|e| {
                e.peer_id == peer_id
                    && e.message_id == message_id
                    && now.saturating_sub(e.cached_at) <= MESSAGE_CACHE_TTL_SECS
            })
            .map(|e| e.attachments.clone())
    }

    /// Кладёт вложения сообщения в короткоживущий кэш.
    ///
    /// Заодно выбрасывает протухшее и держит длину в пределах
    /// [`MESSAGE_CACHE_CAPACITY`] (вытесняется самая старая запись — карта
    /// наполняется запросами браузера и без потолка росла бы неограниченно).
    pub async fn remember_message(
        &self,
        peer_id: i64,
        message_id: i32,
        attachments: Arc<Vec<crate::api::schema::AttachmentRaw>>,
        now: i64,
    ) {
        let mut guard = self.message_cache.lock().await;
        guard.retain(|e| {
            now.saturating_sub(e.cached_at) <= MESSAGE_CACHE_TTL_SECS
                && !(e.peer_id == peer_id && e.message_id == message_id)
        });
        guard.push(CachedMessage {
            peer_id,
            message_id,
            attachments,
            cached_at: now,
        });
        while guard.len() > MESSAGE_CACHE_CAPACITY {
            guard.remove(0);
        }
    }

    /// Забывает сообщение (правка/удаление из LongPoll).
    pub async fn forget_message(&self, peer_id: i64, message_id: i32) {
        self.message_cache
            .lock()
            .await
            .retain(|e| !(e.peer_id == peer_id && e.message_id == message_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VkConfig {
        VkConfig::default()
    }

    fn entry(created_at: i64) -> UploadEntry {
        UploadEntry {
            path: std::path::PathBuf::from("/tmp/x.part"),
            file_name: "x.bin".into(),
            size: 10,
            mime: "application/octet-stream".into(),
            created_at,
        }
    }

    #[tokio::test]
    async fn new_derives_login_from_config() {
        let st = VkState::new(cfg());
        assert_eq!(st.login.lock().await.kind(), "not_configured");
        assert!(!st.is_authorized().await);
        assert!(!st.has_token().await);

        let with_token = VkConfig {
            access_token: SecretToken::new("vk1.a.x"),
            ..cfg()
        };
        let st = VkState::new(with_token);
        assert_eq!(st.login.lock().await.kind(), "disconnected");
        assert!(st.has_token().await);
    }

    #[tokio::test]
    async fn broadcast_event_does_not_panic_without_subscribers() {
        let st = VkState::new(cfg());
        st.broadcast_event(VkServerMsg::Ping);
        st.broadcast_event(VkServerMsg::Resync);
    }

    #[tokio::test]
    async fn subscriber_receives_broadcast() {
        let st = VkState::new(cfg());
        let mut rx = st.subscribe();
        st.broadcast_event(VkServerMsg::Ping);
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, VkServerMsg::Ping);
    }

    #[tokio::test]
    async fn set_login_broadcasts_login_state() {
        let st = VkState::new(cfg());
        let mut rx = st.subscribe();
        st.set_login(LoginFlow::NoMessagesScope {
            error: "VK 15".into(),
        })
        .await;
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match got {
            VkServerMsg::LoginState(dto) => {
                assert_eq!(dto.state, "no_messages_scope");
                assert_eq!(dto.error.as_deref(), Some("VK 15"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(st.login_state_dto().await.state, "no_messages_scope");
    }

    /// Переход в `Authorized` будит ждущего воркера — на этом держится
    /// «вставил токен в работающем процессе и получил realtime без рестарта».
    #[tokio::test]
    async fn set_login_authorized_wakes_waiters() {
        let st = VkState::new(cfg());
        let waiter = {
            let st = st.clone();
            tokio::spawn(async move {
                let notified = st.authorized_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                notified.await;
                st.is_authorized().await
            })
        };
        // Дать задаче дойти до ожидания.
        tokio::time::sleep(Duration::from_millis(20)).await;
        st.set_login(LoginFlow::Authorized {
            user: crate::login::VkUserDto {
                id: "1".into(),
                ..Default::default()
            },
        })
        .await;

        let woke = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("notify не разбудил ждущего")
            .unwrap();
        assert!(woke);
    }

    #[tokio::test]
    async fn state_is_shareable_across_tasks() {
        let st = VkState::new(cfg());
        let clone = st.clone();
        let handle = tokio::spawn(async move { clone.is_authorized().await });
        assert!(!handle.await.unwrap());
    }

    /// Токен, вставленный в UI, обязан быть виден всем — включая
    /// `effective_config`, из которого собирается клиент API.
    #[tokio::test]
    async fn token_updates_are_visible_through_effective_config() {
        let st = VkState::new(cfg());
        assert!(st.effective_config().await.access_token.is_empty());

        st.set_token(SecretToken::new("vk1.a.new")).await;
        assert!(st.has_token().await);
        assert_eq!(
            st.effective_config().await.access_token.expose(),
            "vk1.a.new"
        );

        // Logout возвращает состояние к «не настроено».
        st.set_token(SecretToken::default()).await;
        assert!(!st.has_token().await);
    }

    // ------------------------------------------------------------------
    // ConnHealth.
    // ------------------------------------------------------------------

    /// Полный цикл здоровья: Ok → Down (503 внутри грейса) → по истечении
    /// грейса запросы снова пропускаются → первый успех возвращает Ok.
    #[tokio::test]
    async fn health_gates_requests_only_within_the_grace_window() {
        let st = VkState::new(cfg());
        let t0 = 1_800_000_000;

        // Стартовое здоровье — «пропускаем».
        assert!(st.connected(t0).await);
        assert!(st.down_reason(t0).await.is_none());

        st.note_down(t0, "connection reset").await;
        assert!(!st.connected(t0).await, "сразу после отказа — 503");
        assert!(!st.connected(t0 + HEALTH_GRACE_SECS - 1).await);
        assert_eq!(
            st.down_reason(t0).await.as_deref(),
            Some("connection reset"),
            "текст причины уезжает в message ошибки"
        );

        // Ровно на границе грейса запрос уже пропускается: иначе чинить
        // здоровье было бы нечем — все вызовы отвергались бы на входе.
        assert!(st.connected(t0 + HEALTH_GRACE_SECS).await);
        assert!(st.down_reason(t0 + HEALTH_GRACE_SECS).await.is_none());

        st.note_ok(t0 + HEALTH_GRACE_SECS).await;
        assert!(st.connected(t0 + HEALTH_GRACE_SECS).await);
        assert_eq!(
            st.health().await,
            ConnHealth::Ok {
                at: t0 + HEALTH_GRACE_SECS
            }
        );
    }

    /// Череда отказов не должна продлевать окно 503 бесконечно: `since`
    /// фиксируется первым отказом, иначе здоровье никогда не починится.
    #[tokio::test]
    async fn repeated_failures_do_not_extend_the_window() {
        let st = VkState::new(cfg());
        let t0 = 1_800_000_000;
        st.note_down(t0, "first").await;
        st.note_down(t0 + 1, "second").await;
        st.note_down(t0 + 2, "third").await;

        assert_eq!(
            st.health().await,
            ConnHealth::Down {
                since: t0,
                reason: "first".into()
            },
            "since и причина — от ПЕРВОГО отказа"
        );
        assert!(st.connected(t0 + HEALTH_GRACE_SECS).await);
    }

    // ------------------------------------------------------------------
    // Курсор LongPoll.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn lp_cursor_round_trips_through_state_and_json() {
        let st = VkState::new(cfg());
        assert!(st.lp_cursor().await.is_none());

        let cursor = LpCursor {
            server: "lp.vk.com/wh123".into(),
            key: "abcdef".into(),
            ts: 1_700_000,
            pts: Some(42),
        };
        st.set_lp_cursor(Some(cursor.clone())).await;
        assert_eq!(st.lp_cursor().await.unwrap(), cursor);

        // Курсор персистится на диск — форма обязана переживать сериализацию.
        let json = serde_json::to_string(&cursor).unwrap();
        let back: LpCursor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cursor);

        st.set_lp_cursor(None).await;
        assert!(st.lp_cursor().await.is_none(), "сброс курсора (failed:3)");
    }

    // ------------------------------------------------------------------
    // Воркеры и реестр загрузок.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_workers_is_idempotent_and_survives_an_empty_list() {
        let st = VkState::new(cfg());
        // Ни одного воркера — не виснем.
        st.shutdown_workers().await;

        let token = st.shutdown.clone();
        st.register_worker(tokio::spawn(async move { token.cancelled().await }))
            .await;
        st.shutdown.cancel();
        st.shutdown_workers().await;
        assert!(st.workers.lock().await.is_empty());
        // Повторный вызов безопасен.
        st.shutdown_workers().await;
    }

    /// Воркер, игнорирующий shutdown-токен (завис в бесконечном await):
    /// `shutdown_workers` обязан его abort'нуть после WORKER_STOP_GRACE, а не
    /// зависнуть вместе с ним.
    #[tokio::test]
    async fn shutdown_aborts_worker_ignoring_the_token() {
        let st = VkState::new(cfg());
        st.register_worker(tokio::spawn(std::future::pending::<()>()))
            .await;

        st.shutdown.cancel();
        let done = tokio::time::timeout(
            WORKER_STOP_GRACE + Duration::from_secs(3),
            st.shutdown_workers(),
        )
        .await;
        assert!(done.is_ok(), "shutdown_workers завис на упрямом воркере");
        assert!(st.workers.lock().await.is_empty());
    }

    /// Реестр загрузок: запись кладётся, читается копией, потребляется ровно
    /// один раз (это и есть защита от двойной отправки одного файла).
    #[tokio::test]
    async fn upload_registry_is_consumed_once() {
        let st = VkState::new(cfg());
        st.insert_upload("u1".into(), entry(1_800_000_000)).await;

        assert_eq!(st.upload_entry("u1").await.unwrap().size, 10);
        assert!(st.upload_entry("u1").await.is_some(), "чтение не потребляет");

        assert_eq!(st.take_upload("u1").await.unwrap().file_name, "x.bin");
        assert!(st.take_upload("u1").await.is_none(), "второй раз — нечего");
    }

    /// TTL считается от `created_at` по переданному «сейчас»: тест не спит.
    /// Часы могут прыгнуть назад (NTP) — запись «из будущего» не выбрасывается
    /// и не паникует на вычитании.
    #[tokio::test]
    async fn evict_stale_uploads_uses_injected_now_and_survives_clock_skew() {
        let st = VkState::new(cfg());
        let now = 1_800_000_000;
        st.insert_upload("fresh".into(), entry(now)).await;
        st.insert_upload("edge".into(), entry(now - UPLOAD_TTL_SECS)).await;
        st.insert_upload("old".into(), entry(now - UPLOAD_TTL_SECS - 1))
            .await;
        st.insert_upload("future".into(), entry(now + 3_600)).await;
        st.insert_upload("broken".into(), entry(i64::MIN)).await;

        let gone = st.evict_stale_uploads(UPLOAD_TTL_SECS, now).await;
        assert_eq!(gone.len(), 2, "протухшая и битая: {gone:?}");
        assert!(st.upload_entry("fresh").await.is_some());
        assert!(
            st.upload_entry("edge").await.is_some(),
            "ровно TTL — ещё живая (строгое >)"
        );
        assert!(st.upload_entry("old").await.is_none());
        assert!(
            st.upload_entry("future").await.is_some(),
            "скачок часов не выбрасывает запись"
        );
        assert!(st.upload_entry("broken").await.is_none());
    }

    /// Двойной клик по «Отправить»: несколько задач конкурируют за один
    /// `upload_id`. Победитель ровно один — иначе файл ушёл бы дважды.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_take_upload_has_single_winner() {
        let st = VkState::new(cfg());
        st.insert_upload("race".into(), entry(0)).await;

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

    /// Отставший WS-клиент: буфер переполнен — первый recv отдаёт `Lagged`
    /// (сигнал для resync), после чего канал продолжает работать.
    #[tokio::test]
    async fn slow_subscriber_lags_then_resumes() {
        use tokio::sync::broadcast::error::RecvError;

        let st = VkState::new(cfg());
        let mut rx = st.subscribe();
        for _ in 0..(BROADCAST_CAPACITY + 2) {
            st.broadcast_event(VkServerMsg::Ping);
        }

        match rx.recv().await {
            Err(RecvError::Lagged(n)) => assert!(n >= 1, "пропущено {n}"),
            other => panic!("ожидался Lagged, получено {other:?}"),
        }
        assert_eq!(rx.recv().await.unwrap(), VkServerMsg::Ping);
    }
}
