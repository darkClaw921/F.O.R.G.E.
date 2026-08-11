//! Автомат подключения к порталу: разбор вставленного URL, проверка вебхука и
//! состояния экрана логина.
//!
//! ```text
//!            parse_webhook_input          profile            scope
//! not_configured ─────► (invalid_url)  ─────► (invalid_credentials) ─────► (scope_missing)
//!        │                    │                     │                          │
//!        └──── checking ──────┴─────────────────────┴──────────────────────────┴──► authorized
//!                  │
//!                  ├──► throttled     (портал душит интенсивность, Р16)
//!                  └──► disconnected  (сети нет / портал ответил непонятно)
//! ```
//!
//! # Восемь состояний и ни одним больше
//!
//! [`LoginFlow`] — ЗАМОРОЖЕННЫЙ набор: `not_configured`, `checking`,
//! `invalid_url`, `invalid_credentials`, `scope_missing`, `throttled`,
//! `disconnected`, `authorized`. Машинные имена ([`LoginFlow::kind`]) — контракт
//! с `tmux-web/static/js/bitrix/login.js`: по ним фронт выбирает экран, и
//! переименование обязано валить тест, а не оставлять пользователя перед пустым
//! оверлеем.
//!
//! # Порядок `profile` → `scope` неизменен
//!
//! Сначала выясняется, что вебхук вообще принят порталом (иначе
//! `INVALID_CREDENTIALS`), и только потом — что у него есть нужные права
//! (`insufficient_scope`). Обратный порядок дал бы «не хватает прав» на чужом
//! или опечатанном вебхуке, то есть отправил бы чинить не то. Порядок
//! проверяется тестом по журналу стенда, а не только глазами.
//!
//! # Почему автомат отделён от сети трейтом
//!
//! [`LoginBackend`] — две операции, `profile` и `scope`. Благодаря ему вся
//! матрица переходов (включая ветки троттлинга и потери прав) тестируется без
//! портала, без вебхука и без единого сетевого вызова. Боевая реализация —
//! [`HttpBackend`] поверх [`B24Client`] — не содержит ни одной ветки логики,
//! только перевод ошибок транспорта в [`LoginError`].
//!
//! # Ограничение Р15, которое обязано быть на экране
//!
//! Входящий вебхук выпускается ОТ ИМЕНИ ОДНОГО сотрудника портала:
//! `user_id` криптографически связан с кодом, подменить его невозможно.
//! Следствие для remote-mode: все, кто открыл этот devforge, видят переписку
//! владельца вебхука. Текст предупреждения живёт в
//! [`crate::routes::WEBHOOK_HINT`] и уезжает на экран подключения полем `hint`
//! ответа `GET /api/bitrix/status`.
//!
//! # Что делает logout
//!
//! Стирает вебхук ЛОКАЛЬНО. Отозвать входящий вебхук через REST невозможно —
//! он остаётся действующим, пока владелец не удалит его в интерфейсе портала
//! (Разработчикам → Другое → Входящий вебхук). Делать вид, что мы что-то
//! отозвали на той стороне, было бы враньём в интерфейсе.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::api::error::to_api_error;
use crate::api::methods::{has_im_scope, has_task_scope, SCOPE_IM, SCOPE_TASK};
use crate::api::{B24Client, B24Error};
use crate::config::{is_isolated_env, BitrixConfig, WebhookUrl};
use crate::state::B24State;

/// Права, без которых подключение не считается состоявшимся.
///
/// `im` — мессенджер целиком: без него панель бесполезна. `task` — задачи:
/// директория `~/tasks` и карточка над лентой (Р2) это половина смысла вкладки,
/// а пересоздать вебхук с недостающей галочкой — операция на полминуты.
/// Поэтому оба права требуются НА ВХОДЕ, а не «когда-нибудь всплывёт»:
/// молчаливо деградировавшая панель диагностируется пользователем гораздо
/// дороже, чем честный экран «отметьте эти права».
///
/// Синоним `tasks` у части редакций портала принимается наравне с `task` (см.
/// [`has_task_scope`]).
pub const REQUIRED_SCOPES: [&str; 2] = [SCOPE_IM, SCOPE_TASK];

/// Максимальная длина вставленной строки.
///
/// Живой вебхук — около 60 символов. Мегабайт из буфера обмена разбирать
/// незачем: это не вебхук, а случайная вставка, и отвечать на неё надо сразу.
pub const MAX_INPUT_LEN: usize = 2048;

// ---------------------------------------------------------------------------
// Состояния.
// ---------------------------------------------------------------------------

/// Состояние подключения к порталу.
///
/// Набор состояний — контракт с фронтом: [`LoginFlow::kind`] даёт машинное
/// имя, по которому `js/bitrix/login.js` выбирает экран.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LoginFlow {
    /// Вебхук не задан — стартовое состояние.
    #[default]
    NotConfigured,
    /// Идёт проверка (`profile` → `scope`).
    Checking,
    /// Строка не похожа на URL входящего вебхука.
    InvalidUrl {
        /// Пояснение для пользователя.
        error: Option<String>,
    },
    /// Портал отверг учётные данные (`INVALID_CREDENTIALS` / `NO_AUTH_FOUND`).
    InvalidCredentials {
        /// Пояснение для пользователя.
        error: Option<String>,
    },
    /// У вебхука нет нужных прав (`insufficient_scope`): при создании вебхука
    /// не отмечены «Чат и уведомления» и/или «Задачи».
    ScopeMissing {
        /// Пояснение для пользователя.
        error: Option<String>,
    },
    /// Портал троттлит (Р16). Отдельное состояние, а не `Disconnected`:
    /// реакция UI другая — не «переподключиться», а «подождать».
    Throttled {
        /// Через сколько секунд имеет смысл повторить.
        retry_after_secs: Option<u64>,
    },
    /// Транспортный отказ: сеть, DNS, портал недоступен.
    Disconnected {
        /// Пояснение для пользователя.
        error: Option<String>,
    },
    /// Подключено. `user` — владелец вебхука (Р15: он один на всех, кто
    /// открыл devforge в remote-mode).
    Authorized {
        /// Профиль владельца вебхука.
        user: B24UserDto,
        /// Хост портала без схемы.
        ///
        /// Живёт ТОЛЬКО на сервере: в [`LoginStateDto`] его нет, потому что
        /// `GET /api/bitrix/status` не должен содержать ни кода вебхука, ни
        /// хоста портала ни в одном состоянии. Хост нужен внутри — собирать
        /// ссылки на карточку задачи (Ф10) и писать осмысленный лог.
        portal: String,
    },
}

impl LoginFlow {
    /// Машинное имя состояния — КОНТРАКТ с фронтом.
    ///
    /// Переименование любого значения обязано валить тест, а не тихо
    /// оставлять экран подключения пустым.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Checking => "checking",
            Self::InvalidUrl { .. } => "invalid_url",
            Self::InvalidCredentials { .. } => "invalid_credentials",
            Self::ScopeMissing { .. } => "scope_missing",
            Self::Throttled { .. } => "throttled",
            Self::Disconnected { .. } => "disconnected",
            Self::Authorized { .. } => "authorized",
        }
    }

    /// Подключён ли плагин к порталу прямо сейчас.
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized { .. })
    }

    /// Хост портала, если он известен. Наружу НЕ отдаётся (см.
    /// [`LoginFlow::Authorized`]) — нужен внутренним потребителям.
    pub fn portal(&self) -> Option<&str> {
        match self {
            Self::Authorized { portal, .. } => Some(portal.as_str()),
            _ => None,
        }
    }

    /// Начальное состояние, выводимое ИСКЛЮЧИТЕЛЬНО из конфига.
    ///
    /// Сети не касается: есть вебхук — `disconnected` (проверим фоново), нет —
    /// `not_configured`. Именно поэтому [`crate::init`] синхронный.
    pub fn from_config(cfg: &BitrixConfig) -> Self {
        if cfg.has_webhook() {
            Self::Disconnected { error: None }
        } else {
            Self::NotConfigured
        }
    }

    /// Снимок для REST и WS.
    pub fn to_dto(&self) -> LoginStateDto {
        let (error, retry_after_secs) = match self {
            Self::InvalidUrl { error }
            | Self::InvalidCredentials { error }
            | Self::ScopeMissing { error }
            | Self::Disconnected { error } => (error.clone(), None),
            Self::Throttled { retry_after_secs } => (None, *retry_after_secs),
            _ => (None, None),
        };
        let user = match self {
            Self::Authorized { user, .. } => Some(user.clone()),
            _ => None,
        };
        LoginStateDto {
            state: self.kind().to_string(),
            error,
            retry_after_secs,
            user,
        }
    }
}

/// Владелец вебхука — то, что портал вернул на `profile`.
///
/// Полей намеренно немного: экран подключения показывает «кто вы», а не
/// карточку сотрудника.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct B24UserDto {
    /// `ID` пользователя портала — СТРОКОЙ (единый инвариант id-полей).
    pub id: String,
    /// Имя.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Фамилия.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// Администратор ли портала (`ADMIN`).
    #[serde(default)]
    pub admin: bool,
}

impl B24UserDto {
    /// Проекция сырого ответа `profile` в DTO.
    ///
    /// Пустые строки превращаются в `None`: у портала «нет фамилии» и
    /// «фамилия — пустая строка» это одно и то же, а фронту удобнее отличать
    /// отсутствие значения от значения.
    pub fn from_profile(raw: &crate::api::schema::ProfileRaw) -> Self {
        let opt = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        Self {
            id: raw.id.as_string(),
            name: opt(&raw.name),
            last_name: opt(&raw.last_name),
            admin: raw.admin,
        }
    }
}

/// Снимок состояния подключения для REST (`GET /api/bitrix/status`) и WS
/// (кадр `login_state`).
///
/// # Чего здесь нет
///
/// **Ни кода вебхука, ни хоста портала — ни в одном состоянии.** Код
/// невозможен по типам ([`WebhookUrl`] не реализует `Serialize`), а хост
/// исключён решением: `/status` уходит в браузер, в devtools и в HAR-файл
/// поддержки, и «безобидная» половина секрета сужает перебор до одного
/// портала. Хост живёт внутри [`LoginFlow::Authorized`] и наружу не выходит.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStateDto {
    /// Машинное имя состояния (см. [`LoginFlow::kind`]).
    pub state: String,
    /// Человекочитаемое пояснение, если оно есть.
    #[serde(default)]
    pub error: Option<String>,
    /// Сколько ждать до повтора (только для `throttled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Владелец вебхука (только для `authorized`).
    #[serde(default)]
    pub user: Option<B24UserDto>,
}

// ---------------------------------------------------------------------------
// Ошибки автомата.
// ---------------------------------------------------------------------------

/// Причина, по которой подключение не состоялось.
///
/// Промежуточный тип между транспортом ([`B24Error`]) и экраном
/// ([`LoginFlow`]): он нужен, потому что решение «какой экран показать»
/// принимается на другом уровне, чем «повторять ли запрос», и смешивать их
/// значит превращать `match` в лотерею.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// Вставленная строка не похожа на URL входящего вебхука. Рождается ТОЛЬКО
    /// в [`parse_webhook_input`], то есть ДО любого сетевого вызова.
    BadInput {
        /// Что именно не так — текст показывается пользователю.
        reason: String,
    },
    /// Портал отверг вебхук: `INVALID_CREDENTIALS` / `NO_AUTH_FOUND`.
    InvalidCredentials {
        /// Пояснение.
        message: String,
    },
    /// У вебхука нет нужных прав.
    ScopeMissing {
        /// Пояснение с перечислением недостающих прав.
        message: String,
    },
    /// Портал душит запросы (Р16).
    Throttled {
        /// Сколько ждать до повтора.
        retry_after_secs: Option<u64>,
    },
    /// До портала не доехали.
    Transport {
        /// Пояснение (URL из него уже вычищен транспортом).
        message: String,
    },
    /// Портал ответил отказом по существу либо непонятным телом.
    Portal {
        /// Пояснение.
        message: String,
    },
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadInput { reason } => f.write_str(reason),
            Self::InvalidCredentials { message }
            | Self::ScopeMissing { message }
            | Self::Transport { message }
            | Self::Portal { message } => f.write_str(message),
            Self::Throttled {
                retry_after_secs: Some(secs),
            } => write!(f, "Битрикс24 просит подождать {secs} с"),
            Self::Throttled { .. } => f.write_str("Битрикс24 просит подождать"),
        }
    }
}

impl std::error::Error for LoginError {}

impl LoginError {
    /// Перевод ошибки транспорта в причину отказа подключения.
    ///
    /// Своей таблицы кодов ЗДЕСЬ НЕТ намеренно: разбор идёт через
    /// [`to_api_error`], то есть через ту же замороженную таблицу §4, что
    /// обслуживает все REST-роуты. Второй список кодов неизбежно разъехался бы
    /// с первым, и «неверный вебхук» однажды показался бы как «портал
    /// недоступен».
    pub fn from_b24(err: &B24Error, now: i64) -> Self {
        let api = to_api_error(err, now);
        let message = api.body.message;
        match api.body.code.as_str() {
            "not_authorized" => Self::InvalidCredentials { message },
            "scope_missing" => Self::ScopeMissing { message },
            "flood_wait" => Self::Throttled {
                retry_after_secs: api.body.retry_after_secs,
            },
            "not_connected" => Self::Transport { message },
            _ => Self::Portal { message },
        }
    }
}

// ---------------------------------------------------------------------------
// Разбор вставленного URL.
// ---------------------------------------------------------------------------

/// Разбирает ТРИ формы ввода, которые реально копируют из портала, и приводит
/// их к каноническому виду `https://<портал>/rest/<uid>/<код>/`.
///
/// | что вставили | пример |
/// |---|---|
/// | полный URL с именем метода | `https://p.bitrix24.ru/rest/1/abc123/profile.json` |
/// | URL без метода с хвостовым `/` | `https://p.bitrix24.ru/rest/1/abc123/` |
/// | URL без метода без хвостового `/` | `https://p.bitrix24.ru/rest/1/abc123` |
///
/// Основа — регексп контракта §4:
/// `^(https?://[^/\s]+)/rest/(\d+)/([A-Za-z0-9]+)/?$`, разобранный вручную,
/// чтобы не тащить крейт `regex` ради одной строки. Дополнительно снимаются
/// `?query` и `#fragment` (страница портала показывает адрес со своими
/// параметрами) и хвостовое имя метода.
///
/// **Разбор живёт ТОЛЬКО здесь, на бэкенде.** Второй парсер во фронте был бы
/// вторым местом, где можно ошибиться в обращении с секретом, и он неизбежно
/// разъехался бы с этим.
///
/// # Схема
///
/// Секрет Bitrix24 едет в ПУТИ запроса, поэтому `http://` означает «учётные
/// данные портала уходят по сети открытым текстом». Разрешён он ровно там же,
/// где Р17 разрешает его для `base_url_override`: петлевой хост и только в
/// изолированном (тестовом) окружении.
///
/// # Errors
///
/// [`LoginError::BadInput`] с человекочитаемым пояснением. Само значение в
/// текст ошибки НЕ подставляется: в нём может быть настоящий код вебхука.
pub fn parse_webhook_input(raw: &str) -> Result<WebhookUrl, LoginError> {
    parse_webhook_input_in(raw, is_isolated_env())
}

/// То же, но с явным признаком изолированного окружения — ЧИСТАЯ функция.
///
/// Параметром, а не чтением env внутри: обе ветки правила схемы проверяются
/// юнит-тестами без глобальной подмены переменных процесса.
///
/// # Errors
///
/// См. [`parse_webhook_input`].
pub fn parse_webhook_input_in(raw: &str, isolated: bool) -> Result<WebhookUrl, LoginError> {
    let bad = |reason: &str| LoginError::BadInput {
        reason: reason.to_string(),
    };

    let s = raw.trim();
    if s.is_empty() {
        return Err(bad(
            "вставьте адрес входящего вебхука вида https://ваш-портал.bitrix24.ru/rest/1/код/",
        ));
    }
    if s.len() > MAX_INPUT_LEN {
        return Err(bad("это не похоже на адрес вебхука: слишком длинная строка"));
    }
    // Пользователь копирует адресную строку целиком — вместе с `?IFRAME=Y` и
    // якорем страницы портала.
    let s = s.split(['?', '#']).next().unwrap_or_default();

    let Some(pos) = s.find("/rest/") else {
        return Err(bad(
            "в адресе нет сегмента /rest/ — это не адрес входящего вебхука",
        ));
    };
    let (origin, tail) = s.split_at(pos);
    let host = if let Some(h) = origin.strip_prefix("https://") {
        h
    } else if let Some(h) = origin.strip_prefix("http://") {
        h
    } else {
        return Err(bad("адрес должен начинаться с https://"));
    };
    if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
        return Err(bad("в адресе не разобрался хост портала"));
    }
    if origin.starts_with("http://") {
        let bare = match host.strip_prefix('[') {
            // IPv6-литерал `[::1]:1234`.
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.split(':').next().unwrap_or_default(),
        };
        let loopback = matches!(bare, "127.0.0.1" | "localhost" | "::1");
        if !(loopback && isolated) {
            return Err(bad(
                "адрес должен начинаться с https:// — по http код вебхука уйдёт открытым текстом",
            ));
        }
    }

    let tail = tail.strip_prefix("/rest/").unwrap_or_default();
    let mut segments = tail.split('/');
    let uid = segments.next().unwrap_or_default();
    let code = segments.next().unwrap_or_default();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(
            "после /rest/ ожидается числовой идентификатор пользователя",
        ));
    }
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(bad("после идентификатора пользователя ожидается код вебхука"));
    }

    // Хвост: либо ничего, либо пустой сегмент (адрес со слэшем), либо ОДНО имя
    // метода (`profile.json`, `im.recent.get`) — форма «скопировал вместе с
    // методом из примера в документации».
    let rest: Vec<&str> = segments.collect();
    match rest.as_slice() {
        [] => {}
        [""] => {}
        [method] | [method, ""] => {
            let ok = !method.is_empty()
                && method
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if !ok {
                return Err(bad("после кода вебхука разобрано лишнее — оставьте адрес до кода"));
            }
        }
        _ => {
            return Err(bad(
                "после кода вебхука разобрано лишнее — оставьте адрес до кода",
            ));
        }
    }

    // Каноническая форма ОДНА: со схемой, хостом, uid, кодом и хвостовым
    // слэшем. Именно она уходит в файл конфига, поэтому три разных ввода дают
    // один и тот же байт в байт результат.
    Ok(WebhookUrl::new(format!("{origin}/rest/{uid}/{code}/")))
}

/// Каких прав не хватает вебхуку (в порядке [`REQUIRED_SCOPES`]).
///
/// Пустой список — все права на месте. Пустой `scopes` (портал вернул `[]`)
/// даёт полный список недостающих: «прав нет» — это не «наверное, есть».
pub fn missing_scopes(scopes: &[String]) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !has_im_scope(scopes) {
        missing.push(SCOPE_IM);
    }
    if !has_task_scope(scopes) {
        missing.push(SCOPE_TASK);
    }
    missing
}

/// Текст отказа с перечислением недостающих прав и тем, где их отметить.
pub fn scope_missing_message(missing: &[&'static str]) -> String {
    let list = missing.join(", ");
    format!(
        "у вебхука нет прав: {list}. Пересоздайте входящий вебхук в портале \
         (Разработчикам → Другое → Входящий вебхук), отметив «Чат и уведомления» (im) \
         и «Задачи» (task)"
    )
}

// ---------------------------------------------------------------------------
// Бэкенд проверки.
// ---------------------------------------------------------------------------

/// Две операции проверки подключения, отделённые от сети.
///
/// Ради него в манифесте и живёт `async-trait`: матрица переходов автомата
/// тестируется без портала и без вебхука.
#[async_trait]
pub trait LoginBackend: Send + Sync {
    /// `profile` — «кто владелец вебхука». ПЕРВЫЙ шаг, порядок неизменен.
    ///
    /// # Errors
    ///
    /// [`LoginError::InvalidCredentials`] — вебхук неверен или отозван;
    /// [`LoginError::Transport`] — до портала не доехали;
    /// [`LoginError::Throttled`] — портал душит запросы.
    async fn profile(&self) -> Result<B24UserDto, LoginError>;

    /// `scope` — список прав вебхука. ВТОРОЙ шаг.
    ///
    /// # Errors
    ///
    /// Те же, что у [`LoginBackend::profile`], плюс
    /// [`LoginError::ScopeMissing`], если портал сам отверг вызов по правам.
    async fn scope(&self) -> Result<Vec<String>, LoginError>;
}

/// Боевой бэкенд: обе проверки через [`B24Client`].
///
/// Логики здесь нет вовсе — только перевод [`B24Error`] в [`LoginError`]
/// (см. [`LoginError::from_b24`]).
pub struct HttpBackend {
    client: Arc<B24Client>,
}

impl HttpBackend {
    /// Оборачивает готовый клиент (один на секрет — см.
    /// [`B24State::api_client`]).
    pub fn new(client: Arc<B24Client>) -> Self {
        Self { client }
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }
}

#[async_trait]
impl LoginBackend for HttpBackend {
    async fn profile(&self) -> Result<B24UserDto, LoginError> {
        match self.client.profile().await {
            Ok(raw) => Ok(B24UserDto::from_profile(&raw)),
            Err(e) => Err(LoginError::from_b24(&e, Self::now())),
        }
    }

    async fn scope(&self) -> Result<Vec<String>, LoginError> {
        match self.client.scope().await {
            Ok(scopes) => Ok(scopes),
            Err(e) => Err(LoginError::from_b24(&e, Self::now())),
        }
    }
}

// ---------------------------------------------------------------------------
// Автомат.
// ---------------------------------------------------------------------------

/// Проверяет ТЕКУЩИЙ вебхук состояния и применяет результат к автомату.
///
/// Последовательность: есть ли вебхук → `checking` → `profile` → `scope` →
/// `authorized`. Каждый переход уходит кадром `login_state` всем WS-подписчикам
/// (это делает [`B24State::set_login`]), поэтому оверлей подключения в браузере
/// живёт сам, без опроса `/status`.
///
/// Возвращает итоговое состояние — оно же уже применено к `state`.
pub async fn verify_webhook<B: LoginBackend + ?Sized>(
    state: &Arc<B24State>,
    backend: &B,
) -> LoginFlow {
    if !state.has_webhook().await {
        state.set_login(LoginFlow::NotConfigured).await;
        return LoginFlow::NotConfigured;
    }
    state.set_login(LoginFlow::Checking).await;

    // ШАГ 1. Принят ли вебхук вообще.
    let user = match backend.profile().await {
        Ok(user) => user,
        Err(e) => return apply_login_error(state, e).await,
    };
    // ШАГ 2. Хватает ли прав. Обратный порядок дал бы «не хватает прав» на
    // чужом или опечатанном вебхуке.
    let scopes = match backend.scope().await {
        Ok(scopes) => scopes,
        Err(e) => return apply_login_error(state, e).await,
    };
    let missing = missing_scopes(&scopes);
    if !missing.is_empty() {
        return on_scope_lost(state, scope_missing_message(&missing)).await;
    }

    let portal = state
        .webhook()
        .await
        .portal_host()
        .unwrap_or_else(|| "unknown".to_string());
    tracing::info!(
        target: "forge_bitrix",
        user_id = %user.id,
        portal = %portal,
        "forge-bitrix: webhook verified, the portal accepted it"
    );
    let next = LoginFlow::Authorized { user, portal };
    state.set_login(next.clone()).await;
    next
}

/// То же, но с боевым бэкендом, собранным из состояния.
///
/// Используется стартовой фоновой проверкой ([`crate::spawn_workers`]) и всем,
/// кому нужно «перепроверить то, что уже сохранено». Роут подключения строит
/// клиент сам: ему нужно отличить «клиент не собрался» (400 — вебхук
/// нераспознаваем) от «портал отказал».
///
/// Ошибка сборки клиента здесь трактуется как `invalid_url`: единственная её
/// причина — вебхук, который не разбирается регекспом §4, то есть чинить надо
/// строку, а не сеть.
pub async fn verify_saved_webhook(state: &Arc<B24State>) -> LoginFlow {
    if !state.has_webhook().await {
        state.set_login(LoginFlow::NotConfigured).await;
        return LoginFlow::NotConfigured;
    }
    let client = match state.api_client().await {
        Ok(c) => c,
        Err(e) => {
            // Текст `anyhow`-ошибки не цитирует ни строку вебхука, ни URL —
            // это инвариант `B24Client::new`.
            tracing::warn!(
                target: "forge_bitrix",
                error = %e,
                "forge-bitrix: the saved webhook cannot be used to build a client"
            );
            let next = LoginFlow::InvalidUrl {
                error: Some("сохранённый адрес вебхука не разбирается — вставьте его заново".into()),
            };
            state.set_login(next.clone()).await;
            return next;
        }
    };
    verify_webhook(state, &HttpBackend::new(client)).await
}

/// Переводит автомат в состояние, соответствующее отказу.
///
/// ЕДИНСТВЕННАЯ точка отображения «ошибка → экран», поэтому новый вариант
/// [`LoginError`] невозможно забыть разложить по состояниям: `match`
/// исчерпывающий, а `_ =>` в нём нет.
pub async fn apply_login_error(state: &Arc<B24State>, err: LoginError) -> LoginFlow {
    match err {
        LoginError::BadInput { reason } => {
            // Сюда обычно не попадают: BadInput рождается в
            // parse_webhook_input, то есть ДО применения к состоянию. Ветка
            // существует ради исчерпывающего match.
            let next = LoginFlow::InvalidUrl {
                error: Some(reason),
            };
            state.set_login(next.clone()).await;
            next
        }
        LoginError::InvalidCredentials { message } => on_auth_invalidated(state, message).await,
        LoginError::ScopeMissing { message } => on_scope_lost(state, message).await,
        LoginError::Throttled { retry_after_secs } => on_throttled(state, retry_after_secs).await,
        // «Связи нет» и «портал ответил отказом по существу» показываются одним
        // экраном с разным текстом: действие пользователя в обоих случаях одно
        // — повторить.
        LoginError::Transport { message } | LoginError::Portal { message } => {
            let next = LoginFlow::Disconnected {
                error: Some(message),
            };
            state.set_login(next.clone()).await;
            next
        }
    }
}

/// Портал сказал «вебхук неверен» — [`LoginFlow::InvalidCredentials`].
///
/// Публичная, потому что `INVALID_CREDENTIALS` может прилететь на ЛЮБОМ
/// data-роуте (Ф4+): состояние логина обязано измениться там же, а не ждать
/// следующей ручной проверки.
pub async fn on_auth_invalidated(state: &Arc<B24State>, reason: impl Into<String>) -> LoginFlow {
    let error = reason.into();
    tracing::warn!(
        target: "forge_bitrix",
        reason = %error,
        "forge-bitrix: the portal rejected the webhook"
    );
    let next = LoginFlow::InvalidCredentials { error: Some(error) };
    state.set_login(next.clone()).await;
    next
}

/// Портал сказал «не хватает прав» — [`LoginFlow::ScopeMissing`].
pub async fn on_scope_lost(state: &Arc<B24State>, reason: impl Into<String>) -> LoginFlow {
    let error = reason.into();
    tracing::warn!(
        target: "forge_bitrix",
        reason = %error,
        "forge-bitrix: the webhook lacks the required scopes"
    );
    let next = LoginFlow::ScopeMissing { error: Some(error) };
    state.set_login(next.clone()).await;
    next
}

/// Портал душит запросы (Р16) — [`LoginFlow::Throttled`].
///
/// Отдельное состояние, а не `disconnected`: панель показывает его собственной
/// меткой в статусбаре, потому что лечится оно ожиданием, а не
/// переподключением.
pub async fn on_throttled(state: &Arc<B24State>, retry_after_secs: Option<u64>) -> LoginFlow {
    tracing::warn!(
        target: "forge_bitrix",
        retry_after_secs,
        "forge-bitrix: the portal is throttling the connection check"
    );
    let next = LoginFlow::Throttled { retry_after_secs };
    state.set_login(next.clone()).await;
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BitrixConfig;

    fn cfg() -> BitrixConfig {
        let dir = std::env::temp_dir().join("forge-bitrix-login-tests");
        BitrixConfig {
            cache_dir: dir.join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            ..BitrixConfig::default()
        }
    }

    fn state_with_webhook() -> Arc<B24State> {
        B24State::new(BitrixConfig {
            webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/"),
            ..cfg()
        })
    }

    fn user() -> B24UserDto {
        B24UserDto {
            id: "1".into(),
            name: Some("Игорь".into()),
            last_name: Some("Герасимов".into()),
            admin: true,
        }
    }

    fn full_scopes() -> Vec<String> {
        ["im", "task", "tasks", "tasks_extended"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    // -------------------------------------------------------------------
    // Разбор трёх форм ввода.
    // -------------------------------------------------------------------

    /// ГЛАВНЫЙ ИНВАРИАНТ РАЗБОРА: три формы, которые реально копируют из
    /// портала, дают ОДИН И ТОТ ЖЕ канонический вебхук.
    #[test]
    fn all_three_input_forms_collapse_to_one_canonical_webhook() {
        let canonical = "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/";
        for raw in [
            // 1. Полный URL с именем метода — так он выглядит в примерах доки.
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/profile.json",
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/im.recent.get.json",
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/profile",
            // 2. URL без метода с хвостовым слэшем — так его показывает портал.
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/",
            // 3. То же без слэша.
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4",
            // Плюс мусор буфера обмена: пробелы, перенос строки, ?IFRAME=Y.
            "  https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/  \n",
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/?IFRAME=Y",
            "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/profile.json?auth=x#top",
        ] {
            let w = parse_webhook_input_in(raw, false).unwrap_or_else(|e| panic!("{raw:?}: {e}"));
            assert_eq!(w.expose(), canonical, "вход: {raw:?}");
            assert_eq!(
                w.parts(),
                Some((
                    "https://test-work-2.bitrix24.ru",
                    "1",
                    "wqdzbfhmn83d23j4"
                ))
            );
        }
    }

    #[test]
    fn garbage_is_rejected_without_quoting_the_secret() {
        for raw in [
            "",
            "   ",
            "не урл",
            "просто текст с пробелами",
            "ftp://p.bitrix24.ru/rest/1/abc123/",
            "https://p.bitrix24.ru/",
            "https://p.bitrix24.ru/rest/",
            "https://p.bitrix24.ru/rest/abc/1/",
            "https://p.bitrix24.ru/rest/1/",
            "https://p.bitrix24.ru/rest/1/ab-c/",
            "https:///rest/1/abc123/",
            "https://p.bitrix24.ru/rest/1/abc123/profile.json/extra",
        ] {
            let err = match parse_webhook_input_in(raw, false) {
                Err(e) => e,
                Ok(w) => panic!("принят мусор: {raw:?} → {w:?}"),
            };
            assert!(matches!(err, LoginError::BadInput { .. }), "{raw:?}");
        }
    }

    /// Текст ошибки НЕ цитирует вставленное значение: в нём может быть
    /// настоящий код вебхука с опечаткой в одном символе.
    #[test]
    fn the_error_text_never_quotes_the_input() {
        let err =
            parse_webhook_input_in("https://p.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/a/b/c", false)
                .unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("wqdzbfhmn83d23j4"), "{text}");
        assert!(!text.contains("p.bitrix24.ru"), "{text}");
    }

    /// Схема `http` — только петля и только в изолированном окружении: секрет
    /// Bitrix24 лежит в ПУТИ запроса.
    #[test]
    fn plain_http_is_allowed_only_on_loopback_in_tests() {
        for raw in [
            "http://127.0.0.1:53219/rest/1/abc123/",
            "http://localhost:53219/rest/1/abc123",
            "http://[::1]:53219/rest/1/abc123/",
        ] {
            assert!(parse_webhook_input_in(raw, false).is_err(), "{raw}");
            assert!(parse_webhook_input_in(raw, true).is_ok(), "{raw}");
        }
        // Чужой хост по http отвергается ВСЕГДА.
        for raw in [
            "http://evil.example/rest/1/abc123/",
            "http://192.168.0.10/rest/1/abc123/",
        ] {
            assert!(parse_webhook_input_in(raw, false).is_err(), "{raw}");
            assert!(parse_webhook_input_in(raw, true).is_err(), "{raw}");
        }
    }

    // -------------------------------------------------------------------
    // Права.
    // -------------------------------------------------------------------

    #[test]
    fn missing_scopes_lists_both_required_rights() {
        assert!(missing_scopes(&full_scopes()).is_empty());
        assert_eq!(missing_scopes(&[]), vec!["im", "task"]);
        assert_eq!(missing_scopes(&["im".to_string()]), vec!["task"]);
        assert_eq!(missing_scopes(&["tasks".to_string()]), vec!["im"]);
        // Регистр не имеет значения, синоним `tasks` принимается.
        assert!(missing_scopes(&["IM".to_string(), "TASKS".to_string()]).is_empty());
        // Чужие права не считаются.
        assert_eq!(
            missing_scopes(&["crm".to_string(), "imopenlines".to_string()]),
            vec!["im", "task"]
        );

        let msg = scope_missing_message(&missing_scopes(&[]));
        assert!(msg.contains("im"), "{msg}");
        assert!(msg.contains("task"), "{msg}");
        assert!(msg.contains("Входящий вебхук"), "{msg}");
    }

    // -------------------------------------------------------------------
    // Автомат: подменённый бэкенд, ни одного сетевого вызова.
    // -------------------------------------------------------------------

    /// Бэкенд-заглушка: отдаёт заготовленные результаты и ведёт журнал
    /// вызовов — по нему проверяется ПОРЯДОК `profile` → `scope`.
    struct FakeBackend {
        profile: std::sync::Mutex<Option<Result<B24UserDto, LoginError>>>,
        scope: std::sync::Mutex<Option<Result<Vec<String>, LoginError>>>,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl FakeBackend {
        fn new(
            profile: Result<B24UserDto, LoginError>,
            scope: Result<Vec<String>, LoginError>,
        ) -> Self {
            Self {
                profile: std::sync::Mutex::new(Some(profile)),
                scope: std::sync::Mutex::new(Some(scope)),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn ok() -> Self {
            Self::new(Ok(user()), Ok(full_scopes()))
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LoginBackend for FakeBackend {
        async fn profile(&self) -> Result<B24UserDto, LoginError> {
            self.calls.lock().unwrap().push("profile");
            self.profile
                .lock()
                .unwrap()
                .take()
                .expect("profile вызван дважды")
        }

        async fn scope(&self) -> Result<Vec<String>, LoginError> {
            self.calls.lock().unwrap().push("scope");
            self.scope
                .lock()
                .unwrap()
                .take()
                .expect("scope вызван дважды")
        }
    }

    /// Полный успешный путь + ПОРЯДОК шагов + рассылка переходов в broadcast.
    #[tokio::test]
    async fn a_successful_check_goes_checking_then_profile_then_scope() {
        let st = state_with_webhook();
        let mut rx = st.subscribe();
        let backend = FakeBackend::ok();

        let flow = verify_webhook(&st, &backend).await;
        assert!(flow.is_authorized(), "{flow:?}");
        assert_eq!(flow.portal(), Some("p.bitrix24.ru"));
        assert_eq!(
            backend.calls(),
            vec!["profile", "scope"],
            "порядок проверки обратным быть не может"
        );

        // Клиент увидел ОБА перехода и ровно в том порядке, в каком они были.
        let mut seen = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let crate::ws::protocol::B24ServerMsg::LoginState(dto) = msg {
                seen.push(dto.state);
            }
        }
        assert_eq!(seen, vec!["checking", "authorized"]);

        // Хост портала есть в состоянии, но НЕ в снимке.
        let dto = st.login_state_dto().await;
        assert_eq!(dto.state, "authorized");
        assert_eq!(dto.user.unwrap().id, "1");
        let json = serde_json::to_string(&st.login_state_dto().await).unwrap();
        assert!(!json.contains("p.bitrix24.ru"), "{json}");
    }

    /// Без вебхука проверять нечего — и ни одного вызова бэкенда.
    #[tokio::test]
    async fn without_a_webhook_the_backend_is_not_called_at_all() {
        let st = B24State::new(cfg());
        let backend = FakeBackend::ok();
        let flow = verify_webhook(&st, &backend).await;
        assert_eq!(flow, LoginFlow::NotConfigured);
        assert!(backend.calls().is_empty());
    }

    /// ВСЯ матрица «ошибка шага → экран», по одному тесту на переход.
    #[tokio::test]
    async fn every_failure_maps_to_its_own_screen() {
        // profile отверг вебхук → invalid_credentials, scope НЕ зовётся.
        let st = state_with_webhook();
        let backend = FakeBackend::new(
            Err(LoginError::InvalidCredentials {
                message: "портал отверг вебхук".into(),
            }),
            Ok(full_scopes()),
        );
        let flow = verify_webhook(&st, &backend).await;
        assert_eq!(flow.kind(), "invalid_credentials");
        assert_eq!(backend.calls(), vec!["profile"], "scope звать было незачем");

        // scope отверг вызов по правам → scope_missing.
        let st = state_with_webhook();
        let backend = FakeBackend::new(
            Ok(user()),
            Err(LoginError::ScopeMissing {
                message: "insufficient_scope".into(),
            }),
        );
        assert_eq!(verify_webhook(&st, &backend).await.kind(), "scope_missing");

        // scope вернулся, но прав не хватает → scope_missing с перечислением.
        let st = state_with_webhook();
        let backend = FakeBackend::new(Ok(user()), Ok(vec!["im".to_string()]));
        let flow = verify_webhook(&st, &backend).await;
        assert_eq!(flow.kind(), "scope_missing");
        match flow {
            LoginFlow::ScopeMissing { error: Some(text) } => {
                assert!(text.contains("task"), "{text}");
            }
            other => panic!("{other:?}"),
        }

        // Пустой scope — тоже отказ, а не «наверное, права есть».
        let st = state_with_webhook();
        let backend = FakeBackend::new(Ok(user()), Ok(vec![]));
        assert_eq!(verify_webhook(&st, &backend).await.kind(), "scope_missing");

        // Троттлинг (Р16) — своё состояние, не disconnected.
        let st = state_with_webhook();
        let backend = FakeBackend::new(
            Err(LoginError::Throttled {
                retry_after_secs: Some(7),
            }),
            Ok(full_scopes()),
        );
        let flow = verify_webhook(&st, &backend).await;
        assert_eq!(flow.kind(), "throttled");
        assert_eq!(st.login_state_dto().await.retry_after_secs, Some(7));

        // Сети нет → disconnected.
        let st = state_with_webhook();
        let backend = FakeBackend::new(
            Err(LoginError::Transport {
                message: "connect timeout".into(),
            }),
            Ok(full_scopes()),
        );
        assert_eq!(verify_webhook(&st, &backend).await.kind(), "disconnected");

        // Портал ответил непонятно → тот же экран, другой текст.
        let st = state_with_webhook();
        let backend = FakeBackend::new(
            Err(LoginError::Portal {
                message: "INTERNAL_SERVER_ERROR".into(),
            }),
            Ok(full_scopes()),
        );
        assert_eq!(verify_webhook(&st, &backend).await.kind(), "disconnected");

        // BadInput доходит до автомата только теоретически — но раскладывается.
        let st = state_with_webhook();
        let flow = apply_login_error(
            &st,
            LoginError::BadInput {
                reason: "не адрес вебхука".into(),
            },
        )
        .await;
        assert_eq!(flow.kind(), "invalid_url");
    }

    /// Таблица кодов НЕ дублируется: `LoginError` выводится из §4 через
    /// `to_api_error`.
    #[test]
    fn portal_errors_map_through_the_frozen_table() {
        let now = 1_000;
        let api = |code: &str| {
            LoginError::from_b24(
                &B24Error::Api {
                    code: code.into(),
                    description: String::new(),
                },
                now,
            )
        };
        assert!(matches!(
            api("INVALID_CREDENTIALS"),
            LoginError::InvalidCredentials { .. }
        ));
        assert!(matches!(
            api("NO_AUTH_FOUND"),
            LoginError::InvalidCredentials { .. }
        ));
        assert!(matches!(
            api("insufficient_scope"),
            LoginError::ScopeMissing { .. }
        ));
        assert!(matches!(
            api("INTERNAL_SERVER_ERROR"),
            LoginError::Portal { .. }
        ));
        assert!(matches!(
            LoginError::from_b24(
                &B24Error::Transport {
                    reason: "connect".into()
                },
                now
            ),
            LoginError::Transport { .. }
        ));
        assert!(matches!(
            LoginError::from_b24(&B24Error::Http { status: 502 }, now),
            LoginError::Transport { .. }
        ));
        assert_eq!(
            LoginError::from_b24(&B24Error::Throttled { retry_after_secs: 1 }, now),
            LoginError::Throttled {
                retry_after_secs: Some(1)
            }
        );
        // OPERATION_TIME_LIMIT: пауза считается от абсолютного момента сброса.
        assert_eq!(
            LoginError::from_b24(
                &B24Error::OperatingLimit {
                    reset_at: Some(now + 42)
                },
                now
            ),
            LoginError::Throttled {
                retry_after_secs: Some(42)
            }
        );
    }

    /// Полная таблица «состояние → машинное имя»: переименование обязано
    /// валить тест, а не тихо оставлять фронту пустой экран.
    #[test]
    fn login_kinds_are_a_frozen_contract() {
        let table: [(LoginFlow, &str); 8] = [
            (LoginFlow::NotConfigured, "not_configured"),
            (LoginFlow::Checking, "checking"),
            (LoginFlow::InvalidUrl { error: None }, "invalid_url"),
            (
                LoginFlow::InvalidCredentials { error: None },
                "invalid_credentials",
            ),
            (LoginFlow::ScopeMissing { error: None }, "scope_missing"),
            (
                LoginFlow::Throttled {
                    retry_after_secs: None,
                },
                "throttled",
            ),
            (LoginFlow::Disconnected { error: None }, "disconnected"),
            (
                LoginFlow::Authorized {
                    user: B24UserDto::default(),
                    portal: "p.bitrix24.ru".into(),
                },
                "authorized",
            ),
        ];
        for (flow, kind) in table {
            assert_eq!(flow.kind(), kind);
            assert_eq!(flow.to_dto().state, kind);
        }
    }

    /// DTO сериализуется БЕЗ секрета в любом состоянии — по типам иначе и
    /// быть не может, но проверка дешевле рассуждений.
    #[test]
    fn the_dto_never_carries_the_webhook() {
        let flow = LoginFlow::Authorized {
            user: user(),
            portal: "test-work-2.bitrix24.ru".into(),
        };
        let json = serde_json::to_string(&flow.to_dto()).unwrap();
        assert!(!json.contains("test-work-2.bitrix24.ru"), "{json}");
        assert!(!json.contains("rest/"), "{json}");
        assert!(json.contains("authorized"), "{json}");
    }

    #[test]
    fn profile_projection_turns_empty_strings_into_none() {
        let raw: crate::api::schema::ProfileRaw = serde_json::from_str(
            r#"{"ID":"1","ADMIN":true,"NAME":"Игорь","LAST_NAME":"Герасимов"}"#,
        )
        .unwrap();
        let dto = B24UserDto::from_profile(&raw);
        assert_eq!(dto.id, "1");
        assert_eq!(dto.name.as_deref(), Some("Игорь"));
        assert!(dto.admin);

        let raw: crate::api::schema::ProfileRaw =
            serde_json::from_str(r#"{"ID":19,"NAME":"  ","LAST_NAME":""}"#).unwrap();
        let dto = B24UserDto::from_profile(&raw);
        assert_eq!(dto.id, "19", "числовой ID приводится к строке");
        assert_eq!(dto.name, None);
        assert_eq!(dto.last_name, None);
        assert!(!dto.admin);
    }
}
