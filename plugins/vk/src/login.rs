//! Автомат состояний логина VK.
//!
//! Здесь только ЧИСТАЯ половина: перечисление состояний, их машинные имена и
//! сериализуемая проекция. Сетевые переходы (`users.get` →
//! `messages.getConversations`, разбор вставленного redirect-URL, captcha)
//! приезжают в Ф3 вместе с `trait LoginBackend` и `verify_token` — граница
//! проведена ровно там же, где у Telegram, чтобы матрица переходов
//! тестировалась без сети и без живого токена.
//!
//! ## Почему состояний семь, а не пять
//!
//! ```text
//!   not_configured ──вставили токен──▶ checking ──┬─▶ authorized
//!         ▲                                       ├─▶ invalid_token      (VK 5)
//!         │                                       ├─▶ no_messages_scope  (VK 15)
//!         └──logout─── любое состояние            ├─▶ captcha_required   (VK 14)
//!                                                 └─▶ disconnected       (сеть/5xx)
//! ```
//!
//! Два состояния добавлены против Telegram, и оба — не косметика:
//!
//! - **`no_messages_scope`** (VK код 15). VK не выдаёт право `messages`
//!   обычным standalone-приложениям, и рабочие токены пользователи добывают
//!   под app_id сторонних клиентов. Это ОТДЕЛЬНЫЙ экран: увидев вместо него
//!   «что-то пошло не так», пользователь будет вставлять один и тот же
//!   рабочий токен по кругу. Отличается от `invalid_token` тем, что токен
//!   исправен — не хватает именно права.
//! - **`checking`**. Проверка токена — сетевой вызов из `spawn_workers`, и
//!   без отдельного состояния UI мигал бы «отключено» на каждом старте.
//!
//! `captcha_required` живёт здесь же, но captcha у VK прилетает на ЛЮБОЙ
//! метод, а не только на логин, поэтому её оверлей во фронте вынесен из
//! оверлея логина (`#vk-captcha`, слой 7).
//!
//! ## Как токен попадает в плагин
//!
//! Пользователь получает токен через Implicit Flow и КОПИРУЕТ АДРЕСНУЮ СТРОКУ
//! ЦЕЛИКОМ:
//!
//! ```text
//! https://oauth.vk.ru/blank.html#access_token=vk1.a.XXXX&expires_in=0&user_id=105431859&email=…
//! ```
//!
//! [`parse_token_input`] принимает три формы (полный URL, голый фрагмент,
//! просто токен) и живёт ТОЛЬКО здесь: вторая копия разбора на фронте была бы
//! вторым местом, где можно ошибиться в обращении с секретом. Требовать от
//! человека «выделите подстроку между `access_token=` и `&`» означает
//! перекладывать на него работу парсера и получать поддержку вида «вставил, не
//! работает».

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::api::error::{
    VkError, CODE_ACCESS_DENIED, CODE_AUTH_FAILED, CODE_CAPTCHA, CODE_FLOOD_CONTROL,
    CODE_METHOD_QUOTA, RETRY_AFTER_FLOOD_CONTROL, RETRY_AFTER_METHOD_QUOTA,
};
use crate::api::methods::{Captcha, USER_FIELDS};
use crate::api::schema::ProfileRaw;
use crate::api::{Params, VkClient};
use crate::config::{SecretToken, VkConfig};
use crate::state::VkState;

/// Минимальная длина строки, которую мы согласны считать access-токеном.
///
/// Живой токен VK — это ~200 символов; всё, что короче 8, — почти наверняка
/// обрывок копирования (или вовсе не токен). Отказ на входе честнее, чем
/// сохранить мусор в `vk.toml` и показать пользователю «VK 5» после сетевого
/// вызова.
pub const MIN_TOKEN_LEN: usize = 8;

/// Верхняя граница длины токена — защита от вставки всего буфера обмена.
pub const MAX_TOKEN_LEN: usize = 4096;

/// Разобранный ввод пользователя.
///
/// `Serialize` НЕ выводится и не будет: структура несёт [`SecretToken`], а он
/// не сериализуем по построению.
///
/// **`email` из фрагмента сюда не попадает вовсе** — поля под него нет. Адрес
/// не нужен ни для одного вызова VK, и хранить чужой email без причины
/// незачем: чего нет в структуре, то не утечёт ни в лог, ни в файл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTokenInput {
    /// Сам токен.
    pub token: SecretToken,
    /// `user_id` из фрагмента — ПОДСКАЗКА, а не истина.
    ///
    /// Источник истины — `users.get` по самому токену: токен мог быть выдан
    /// другому аккаунту, чем тот, чей адрес скопировали (типичный случай — два
    /// профиля в одном браузере).
    pub user_id: Option<i64>,
    /// `expires_in` из фрагмента, секунды. `0` (и отсутствие поля) означает
    /// бессрочный токен — так VK отвечает на scope `offline`.
    pub expires_in: Option<u64>,
}

impl ParsedTokenInput {
    /// Бессрочен ли токен (scope `offline`).
    ///
    /// Отсутствие `expires_in` трактуется как «бессрочный»: голый токен без
    /// фрагмента ничего о сроке не сообщает, и выдумывать ему протухание —
    /// значит показывать пользователю предупреждение на пустом месте.
    pub fn is_permanent(&self) -> bool {
        matches!(self.expires_in, None | Some(0))
    }

    /// Unix-время протухания, если срок известен и конечен.
    ///
    /// `now` передаётся снаружи, чтобы функция оставалась чистой и
    /// тестировалась без часов.
    pub fn expires_at(&self, now: i64) -> Option<i64> {
        match self.expires_in {
            Some(secs) if secs > 0 => Some(now.saturating_add(i64::try_from(secs).unwrap_or(i64::MAX))),
            _ => None,
        }
    }
}

/// Отказ сетевой (и разбирающей) половины логина.
///
/// Отдельный тип, а не [`VkError`], по той же причине, по которой у REST свой
/// контракт ошибок: транспорт знает про ретраи, автомат логина — про ЭКРАНЫ.
/// Каждый вариант ниже соответствует ровно одному состоянию [`LoginFlow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// Вставленная строка не содержит токена (или он неправдоподобен).
    ///
    /// В сообщение НИКОГДА не попадает сама строка: она и есть секрет.
    BadInput {
        /// Что именно не так — без цитирования ввода.
        reason: String,
    },
    /// VK код 5: токен протух, отозван или скопирован с ошибкой.
    InvalidToken {
        /// Текст для пользователя.
        message: String,
    },
    /// VK код 15 на `messages.*`: у токена нет права `messages`.
    NoMessagesScope {
        /// Текст для пользователя.
        message: String,
    },
    /// VK код 14: требуется ввод символов с картинки.
    Captcha {
        /// `captcha_sid`.
        sid: String,
        /// URL картинки у VK (фронт берёт её через прокси бэкенда).
        img: String,
    },
    /// До VK не доехали: сеть, 5xx, таймаут. Лечится повтором.
    Transport {
        /// Текст для пользователя.
        message: String,
    },
    /// Прочий отказ VK — повтор его не исправит, но и экрана под него нет.
    Vk {
        /// Текст для пользователя.
        message: String,
    },
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadInput { reason } => write!(f, "{reason}"),
            Self::InvalidToken { message }
            | Self::NoMessagesScope { message }
            | Self::Transport { message }
            | Self::Vk { message } => f.write_str(message),
            Self::Captcha { sid, .. } => write!(f, "VK requires a captcha (sid {sid})"),
        }
    }
}

impl std::error::Error for LoginError {}

impl From<&VkError> for LoginError {
    /// Отображение транспортной ошибки в отказ логина.
    ///
    /// Коды 5 / 14 / 15 — три ветки, ради которых автомат вообще существует;
    /// остальное делится на «связи нет» (повторяемо) и «VK отказал».
    fn from(err: &VkError) -> Self {
        match err {
            VkError::Transport { reason } => Self::Transport {
                message: format!("VK is unreachable: {reason}"),
            },
            VkError::Http { status } => Self::Transport {
                message: format!("api.vk.com replied with HTTP {status}"),
            },
            VkError::Decode { reason } => Self::Vk {
                message: format!("could not parse the VK response: {reason}"),
            },
            VkError::Api {
                code,
                msg,
                captcha_sid,
                captcha_img,
                ..
            } => match *code {
                CODE_AUTH_FAILED => Self::InvalidToken {
                    message: with_vk_text(
                        "the VK token is invalid, expired or revoked",
                        *code,
                        msg,
                    ),
                },
                CODE_ACCESS_DENIED => Self::NoMessagesScope {
                    message: with_vk_text(
                        "the VK token has no `messages` permission (VK does not grant it to standard standalone apps)",
                        *code,
                        msg,
                    ),
                },
                CODE_CAPTCHA => match (captcha_sid, captcha_img) {
                    (Some(sid), Some(img)) => Self::Captcha {
                        sid: sid.clone(),
                        img: img.clone(),
                    },
                    // Код 14 без sid/img показать нечем: честнее общий отказ,
                    // чем оверлей с пустой картинкой.
                    _ => Self::Vk {
                        message: with_vk_text("VK requires a captcha", *code, msg),
                    },
                },
                // Коды 9 и 29 — «слишком часто», а НЕ «токен плохой».
                // Общий текст «VK rejected the request» читается как отказ по
                // существу, и человек идёт добывать новый токен вместо того,
                // чтобы просто подождать. Ведут они в тот же экран
                // `disconnected`, что и обрыв связи, — действие пользователя
                // ровно то же (повторить), поэтому вариант `Transport`, а не
                // отдельное состояние.
                CODE_FLOOD_CONTROL => Self::Transport {
                    message: with_vk_text(
                        &format!(
                            "VK has temporarily throttled requests from this token (flood control); \
                             the token itself is fine — wait about {} seconds and press CONNECT again",
                            RETRY_AFTER_FLOOD_CONTROL
                        ),
                        *code,
                        msg,
                    ),
                },
                CODE_METHOD_QUOTA => Self::Transport {
                    message: with_vk_text(
                        &format!(
                            "the hourly quota of this VK method is spent; the token itself is fine — \
                             wait up to {} minutes and press CONNECT again",
                            RETRY_AFTER_METHOD_QUOTA / 60
                        ),
                        *code,
                        msg,
                    ),
                },
                _ => Self::Vk {
                    message: with_vk_text("VK rejected the request", *code, msg),
                },
            },
        }
    }
}

/// Подмешивает дословный текст VK к каноническому сообщению.
///
/// Секрета в нём нет: `request_params` вырезаны ещё в
/// [`crate::api::parse_envelope`].
fn with_vk_text(canonical: &str, code: i64, msg: &str) -> String {
    if msg.is_empty() {
        format!("{canonical} (VK error {code})")
    } else {
        format!("{canonical} (VK error {code}: {msg})")
    }
}

/// Разбирает то, что пользователь вставил в поле логина.
///
/// Принимаются ТРИ формы:
///
/// | вход | что делаем |
/// |---|---|
/// | `https://oauth.vk.ru/blank.html#access_token=…&expires_in=0&user_id=1&email=…` | берём фрагмент после первого `#` |
/// | `access_token=…&expires_in=0&user_id=1` | разбираем как фрагмент |
/// | `vk1.a.…` | считаем голым токеном |
///
/// Дополнительно: URL с `?` вместо `#` (VK так отвечает, если приложение
/// сконфигурировано на code flow) — параметры берутся после первого `?`.
///
/// Правила:
///
/// - **пробелы и переносы строк вырезаются ОТОВСЮДУ**, а не только по краям:
///   адресная строка, скопированная из терминала или из письма, приезжает
///   разорванной по 80 колонок, и `trim()` тут не помогает. В самом токене
///   пробелов не бывает — терять нечего;
/// - **`email` не читается вовсе** (см. [`ParsedTokenInput`]);
/// - `user_id` и `expires_in` — необязательные подсказки; мусор в них
///   игнорируется, а не роняет разбор: без токена жить нельзя, без `user_id`
///   можно;
/// - функция ЧИСТАЯ: не паникует, не ходит в сеть и **не пишет НИ ОДНОЙ строки
///   в лог** — входная строка целиком является секретом.
///
/// # Errors
///
/// [`LoginError::BadInput`] — пустой ввод, фрагмент без `access_token`,
/// слишком короткий/длинный токен, недопустимые символы (в том числе URL без
/// фрагмента вовсе).
pub fn parse_token_input(raw: &str) -> Result<ParsedTokenInput, LoginError> {
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err(bad_input("paste the address bar contents or the access token itself"));
    }

    // Фрагмент важнее query: в `blank.html?foo=1#access_token=…` токен лежит
    // после `#`, и проверка `?` первой увела бы разбор не туда.
    let params = match cleaned.find('#') {
        Some(idx) => &cleaned[idx + 1..],
        None => match cleaned.find('?') {
            Some(idx) => &cleaned[idx + 1..],
            None => cleaned.as_str(),
        },
    };

    if !params.contains('=') {
        // Голый токен: ни фрагмента, ни пар `ключ=значение`.
        return finish(params, None, None);
    }

    let mut token: Option<&str> = None;
    let mut user_id: Option<i64> = None;
    let mut expires_in: Option<u64> = None;
    for pair in params.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "access_token" => token = Some(value),
            "user_id" => user_id = value.parse::<i64>().ok(),
            "expires_in" => expires_in = value.parse::<u64>().ok(),
            // `email` и всё прочее ОТБРАСЫВАЕТСЯ ЗДЕСЬ И НАВСЕГДА.
            _ => {}
        }
    }

    let Some(token) = token else {
        return Err(bad_input(
            "the pasted address has no `access_token=` part — copy the whole address bar after the login page opens",
        ));
    };
    finish(token, user_id, expires_in)
}

/// Проверяет кандидата в токены и собирает результат.
fn finish(
    candidate: &str,
    user_id: Option<i64>,
    expires_in: Option<u64>,
) -> Result<ParsedTokenInput, LoginError> {
    if candidate.is_empty() {
        return Err(bad_input("the access token is empty"));
    }
    if candidate.len() > MAX_TOKEN_LEN {
        return Err(bad_input("the pasted value is too long to be an access token"));
    }
    if candidate.len() < MIN_TOKEN_LEN {
        return Err(bad_input("the pasted value is too short to be an access token"));
    }
    if !candidate
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        // Сюда попадает и «вставили голый URL без фрагмента»: в нём есть `:`
        // и `/`, которых в токене не бывает.
        return Err(bad_input(
            "the pasted value does not look like an access token (expected letters, digits, `.`, `_`, `-`)",
        ));
    }
    Ok(ParsedTokenInput {
        token: SecretToken::new(candidate),
        user_id,
        expires_in,
    })
}

/// Собирает [`LoginError::BadInput`]. Ввод в текст НЕ подставляется.
fn bad_input(reason: &str) -> LoginError {
    LoginError::BadInput {
        reason: reason.to_string(),
    }
}

/// Авторизованный пользователь в проекции для UI.
///
/// Источник истины — `users.get` по токену, а НЕ `user_id` из вставленного
/// redirect-URL: токен мог быть выдан другому аккаунту, чем тот, чей адрес
/// скопировали.
///
/// Аватар (`photo_100`) намеренно не переносится: это ссылка на CDN VK, а
/// прямым ссылкам VK в DOM браузера делать нечего (всё медиа идёт через
/// прокси бэкенда). Понадобится — приедет отдельным полем-прокси в Ф7.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VkUserDto {
    /// Числовой id аккаунта строкой (инвариант «id — строки»).
    pub id: String,
    /// Имя.
    pub first_name: Option<String>,
    /// Фамилия.
    pub last_name: Option<String>,
    /// Короткое имя (`screen_name`), если задано.
    pub screen_name: Option<String>,
}

impl VkUserDto {
    /// Отображаемое имя одной строкой: «Имя Фамилия», иначе `@screen_name`,
    /// иначе `id`. Нужна ровно одна реализация — фронт показывает результат
    /// в титлбаре и в тостах.
    pub fn display_name(&self) -> String {
        let full = [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !full.is_empty() {
            return full;
        }
        match self.screen_name.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => format!("@{s}"),
            _ => self.id.clone(),
        }
    }
}

/// Сериализуемая проекция [`LoginFlow`] — единственное, что уходит клиенту
/// (REST `GET /api/vk/status` и WS-кадр `login_state`).
///
/// Поля токена здесь нет и быть не может ПО ТИПАМ: `SecretToken` не
/// реализует `Serialize`, поэтому любая попытка его сюда добавить перестанет
/// компилироваться.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStateDto {
    /// Машинное имя состояния (см. [`LoginFlow::kind`]) — контракт с фронтом.
    pub state: String,
    /// Текст последней ошибки (в `disconnected`, `invalid_token`,
    /// `no_messages_scope`).
    pub error: Option<String>,
    /// Авторизованный пользователь (только в `authorized`).
    pub user: Option<VkUserDto>,
    /// Идентификатор captcha (только в `captcha_required`).
    pub captcha_sid: Option<String>,
    /// URL картинки captcha у VK (только в `captcha_required`).
    ///
    /// Фронт этот URL в `<img src>` НЕ ставит — картинка забирается через
    /// прокси бэкенда, чтобы браузер пользователя не ходил на серверы VK.
    pub captcha_img: Option<String>,
}

/// Состояние авторизации плагина.
///
/// Живёт под `Mutex` в [`crate::state::VkState`]; каждый переход
/// broadcast'ит `login_state` всем WS-подписчикам, чтобы открытые вкладки
/// показывали одинаковую картину.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFlow {
    /// Токена нет — UI показывает форму вставки.
    NotConfigured,
    /// Токен есть, но он ещё не проверен сетью (старт процесса или ручной
    /// повтор). Промежуточное состояние, без него UI мигал бы «офлайн».
    Checking,
    /// Токен есть, но связи нет: сеть, 5xx, таймаут. Лечится повтором.
    Disconnected {
        /// Причина последнего сбоя, если она известна.
        error: Option<String>,
    },
    /// VK код 5 — токен протух, отозван или скопирован с ошибкой.
    InvalidToken {
        /// Пояснение для пользователя.
        error: String,
    },
    /// VK код 15 на `messages.*` — токен рабочий, но без права `messages`.
    NoMessagesScope {
        /// Пояснение для пользователя (ссылка на способ добыть токен).
        error: String,
    },
    /// VK код 14 — требуется ввод символов с картинки.
    CaptchaRequired {
        /// `captcha_sid` из ответа VK, нужен для повтора запроса.
        sid: String,
        /// URL картинки у VK; фронт берёт её через прокси бэкенда.
        img: String,
    },
    /// Токен проверен, аккаунт доступен.
    Authorized {
        /// Кто мы (из `users.get`).
        user: VkUserDto,
    },
}

impl LoginFlow {
    /// Начальное состояние из конфига: токен есть ⇒ [`LoginFlow::Disconnected`]
    /// (проверять его будет `spawn_workers`), нет ⇒ `NotConfigured`.
    ///
    /// Именно `Disconnected`, а не `Checking`: `init` синхронный и сети не
    /// трогает, а `Checking` означало бы «проверка уже идёт».
    pub fn from_config(cfg: &VkConfig) -> Self {
        if cfg.has_token() {
            Self::Disconnected { error: None }
        } else {
            Self::NotConfigured
        }
    }

    /// Машинное имя состояния — ровно то, что видит фронт в поле `state`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Checking => "checking",
            Self::Disconnected { .. } => "disconnected",
            Self::InvalidToken { .. } => "invalid_token",
            Self::NoMessagesScope { .. } => "no_messages_scope",
            Self::CaptchaRequired { .. } => "captcha_required",
            Self::Authorized { .. } => "authorized",
        }
    }

    /// Есть ли рабочий доступ к аккаунту прямо сейчас.
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized { .. })
    }

    /// Настроен ли плагин (есть ли смысл что-то проверять).
    ///
    /// `false` только у [`LoginFlow::NotConfigured`]: во всех остальных
    /// состояниях токен в конфиге есть, пусть и неисправный.
    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::NotConfigured)
    }

    /// Проекция для REST/WS.
    pub fn to_dto(&self) -> LoginStateDto {
        let mut dto = LoginStateDto {
            state: self.kind().to_string(),
            ..LoginStateDto::default()
        };
        match self {
            Self::Disconnected { error } => dto.error = error.clone(),
            Self::InvalidToken { error } | Self::NoMessagesScope { error } => {
                dto.error = Some(error.clone());
            }
            Self::CaptchaRequired { sid, img } => {
                dto.captcha_sid = Some(sid.clone());
                dto.captcha_img = Some(img.clone());
            }
            Self::Authorized { user } => dto.user = Some(user.clone()),
            Self::NotConfigured | Self::Checking => {}
        }
        dto
    }
}

// ---------------------------------------------------------------------------
// Сетевая половина.
// ---------------------------------------------------------------------------

/// Две проверки, из которых состоит верификация токена.
///
/// Трейт существует ровно затем, чтобы МАТРИЦА ПЕРЕХОДОВ автомата
/// тестировалась без сети и без живого токена: подсовываем фейковый бэкенд,
/// который отвечает заранее заданным [`LoginError`], и проверяем, в какое
/// состояние приехал [`LoginFlow`]. Боевая реализация — [`HttpBackend`].
#[async_trait]
pub trait LoginBackend: Send + Sync {
    /// `users.get` без идентификаторов — «расскажи о владельце токена».
    ///
    /// # Errors
    ///
    /// [`LoginError::InvalidToken`] на коде 5, [`LoginError::Captcha`] на 14,
    /// [`LoginError::Transport`] при сетевом отказе.
    async fn whoami(&self, captcha: Option<&Captcha>) -> Result<VkUserDto, LoginError>;

    /// `messages.getConversations(count=1)` — проба права `messages`.
    ///
    /// Метод выбран самым дешёвым из тех, что требуют этого права: одна беседа
    /// без `extended`, то есть без дозапроса профилей.
    ///
    /// # Errors
    ///
    /// [`LoginError::NoMessagesScope`] на коде 15 — то, ради чего проба и
    /// существует.
    async fn probe_messages(&self, captcha: Option<&Captcha>) -> Result<(), LoginError>;
}

/// Боевой бэкенд: обе проверки через [`VkClient`].
///
/// Запросы собираются ЗДЕСЬ, а не через [`crate::api::methods`], по одной
/// причине: `users.get` и `messages.getConversations` там не принимают
/// captcha (в Ф2 она заведена только у мутирующих методов), а повтор проверки
/// после ввода символов — это тот же запрос плюс `captcha_sid`+`captcha_key`.
/// Заодно проба обходится без `extended=1`: на этом шаге профили участников
/// не нужны никому.
pub struct HttpBackend {
    client: Arc<VkClient>,
}

impl HttpBackend {
    /// Оборачивает готовый клиент (один на токен — см.
    /// [`VkState::api_client`]).
    pub fn new(client: Arc<VkClient>) -> Self {
        Self { client }
    }

    /// Добавляет ответ на captcha в параметры «как есть».
    fn push_captcha(params: &mut Params, captcha: Option<&Captcha>) {
        if let Some(c) = captcha {
            params.push(("captcha_sid", c.sid.clone()));
            params.push(("captcha_key", c.key.clone()));
        }
    }
}

#[async_trait]
impl LoginBackend for HttpBackend {
    async fn whoami(&self, captcha: Option<&Captcha>) -> Result<VkUserDto, LoginError> {
        let mut params: Params = vec![("fields", USER_FIELDS.to_string())];
        Self::push_captcha(&mut params, captcha);
        let value = self
            .client
            .call_value("users.get", params)
            .await
            .map_err(|e| LoginError::from(&e))?;

        // `users.get` отдаёт массив; у v5.147+ он бывает завёрнут в `items`.
        let items = if value.is_array() {
            value
        } else {
            value.get("items").cloned().unwrap_or(value)
        };
        let profiles: Vec<ProfileRaw> = serde_json::from_value(items).map_err(|e| LoginError::Vk {
            message: format!("could not parse the VK profile: {e}"),
        })?;
        let me = profiles.into_iter().next().ok_or_else(|| LoginError::Vk {
            message: "VK returned no profile for this token".to_string(),
        })?;
        Ok(VkUserDto::from(&me))
    }

    async fn probe_messages(&self, captcha: Option<&Captcha>) -> Result<(), LoginError> {
        let mut params: Params = vec![
            ("count", "1".to_string()),
            ("filter", "all".to_string()),
        ];
        Self::push_captcha(&mut params, captcha);
        self.client
            .call_value("messages.getConversations", params)
            .await
            .map_err(|e| LoginError::from(&e))?;
        Ok(())
    }
}

impl From<&ProfileRaw> for VkUserDto {
    fn from(p: &ProfileRaw) -> Self {
        let opt = |s: &str| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        Self {
            id: p.id.to_string(),
            first_name: opt(&p.first_name),
            last_name: opt(&p.last_name),
            screen_name: opt(&p.screen_name),
        }
    }
}

/// Проверяет токен и ПЕРЕВОДИТ автомат в итоговое состояние.
///
/// Порядок проверок зафиксирован и обратным быть не может:
///
/// 1. [`LoginBackend::whoami`] — дёшево и покажет код 5;
/// 2. [`LoginBackend::probe_messages`] — покажет код 15.
///
/// **Обратный порядок дал бы «нет права messages» на протухшем токене**: VK на
/// `messages.*` с мёртвым токеном отвечает 5, но пользователь, увидевший
/// экран про scope, полез бы добывать новый токен с другими правами вместо
/// того, чтобы просто обновить старый.
///
/// Перед первым вызовом состояние становится [`LoginFlow::Checking`] — без
/// него UI мигал бы «отключено» на каждом старте процесса.
///
/// Возвращает итоговое состояние, чтобы REST-хендлер отдал ровно его, не
/// перечитывая состояние вторым запросом (между записью и чтением успел бы
/// вклиниться другой переход).
pub async fn verify_token<B: LoginBackend + ?Sized>(
    state: &Arc<VkState>,
    backend: &B,
    captcha: Option<&Captcha>,
) -> LoginFlow {
    if !state.has_token().await {
        state.set_login(LoginFlow::NotConfigured).await;
        return LoginFlow::NotConfigured;
    }
    state.set_login(LoginFlow::Checking).await;

    let user = match backend.whoami(captcha).await {
        Ok(user) => user,
        Err(e) => return apply_login_error(state, e).await,
    };
    if let Err(e) = backend.probe_messages(captcha).await {
        return apply_login_error(state, e).await;
    }

    tracing::info!(
        target: "forge_vk",
        user_id = %user.id,
        "forge-vk: token verified, account is authorized"
    );
    let next = LoginFlow::Authorized { user };
    state.set_login(next.clone()).await;
    next
}

/// Переводит автомат в состояние, соответствующее отказу.
///
/// Единственная точка отображения «ошибка → экран», поэтому новый вариант
/// [`LoginError`] невозможно забыть разложить по состояниям.
async fn apply_login_error(state: &Arc<VkState>, err: LoginError) -> LoginFlow {
    match err {
        LoginError::InvalidToken { message } => on_auth_invalidated(state, message).await,
        LoginError::NoMessagesScope { message } => on_scope_lost(state, message).await,
        LoginError::Captcha { sid, img } => on_captcha(state, sid, img).await,
        // «Связи нет» и «VK отказал по существу» показываются одним экраном с
        // разным текстом: отдельное состояние под второй случай пришлось бы
        // рисовать во фронте, а действие пользователя в обоих одно — повторить.
        LoginError::Transport { message } | LoginError::Vk { message } => {
            let next = LoginFlow::Disconnected {
                error: Some(message),
            };
            state.set_login(next.clone()).await;
            next
        }
        // Сюда попасть нельзя: BadInput рождается только в parse_token_input,
        // то есть ДО любого сетевого вызова. Ветка существует ради
        // исчерпывающего match — молчаливый `_ =>` скрыл бы новый вариант.
        LoginError::BadInput { reason } => {
            let next = LoginFlow::InvalidToken { error: reason };
            state.set_login(next.clone()).await;
            next
        }
    }
}

/// VK сказал «токен невалиден» (код 5) — переводит автомат в
/// [`LoginFlow::InvalidToken`].
///
/// Публичная, потому что тот же код 5 может прилететь на ЛЮБОМ data-роуте
/// (Ф4+): состояние логина обязано измениться там же, а не ждать следующей
/// ручной проверки.
pub async fn on_auth_invalidated(state: &Arc<VkState>, reason: impl Into<String>) -> LoginFlow {
    let error = reason.into();
    tracing::warn!(target: "forge_vk", reason = %error, "forge-vk: token rejected by VK");
    let next = LoginFlow::InvalidToken { error };
    state.set_login(next.clone()).await;
    next
}

/// VK сказал «нет права `messages`» (код 15) — [`LoginFlow::NoMessagesScope`].
pub async fn on_scope_lost(state: &Arc<VkState>, reason: impl Into<String>) -> LoginFlow {
    let error = reason.into();
    tracing::warn!(target: "forge_vk", reason = %error, "forge-vk: token has no `messages` scope");
    let next = LoginFlow::NoMessagesScope { error };
    state.set_login(next.clone()).await;
    next
}

/// VK потребовал captcha (код 14) — [`LoginFlow::CaptchaRequired`].
///
/// Заодно ЗАПОМИНАЕТ адрес картинки: фронт грузит её через прокси
/// `GET /api/vk/captcha/:sid`, и другого источника URL у прокси нет (браузер
/// на серверы VK не ходит, а `sid` приходит от него же).
pub async fn on_captcha(state: &Arc<VkState>, sid: String, img: String) -> LoginFlow {
    tracing::warn!(target: "forge_vk", sid = %sid, "forge-vk: VK requires a captcha");
    state.remember_captcha(&sid, &img).await;
    let next = LoginFlow::CaptchaRequired { sid, img };
    state.set_login(next.clone()).await;
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretToken;
    use crate::ws::protocol::VkServerMsg;

    fn all_states() -> Vec<LoginFlow> {
        vec![
            LoginFlow::NotConfigured,
            LoginFlow::Checking,
            LoginFlow::Disconnected { error: None },
            LoginFlow::Disconnected {
                error: Some("connection reset".into()),
            },
            LoginFlow::InvalidToken {
                error: "VK 5".into(),
            },
            LoginFlow::NoMessagesScope {
                error: "VK 15".into(),
            },
            LoginFlow::CaptchaRequired {
                sid: "239123".into(),
                img: "https://api.vk.com/captcha.php?sid=239123".into(),
            },
            LoginFlow::Authorized {
                user: VkUserDto {
                    id: "105431859".into(),
                    first_name: Some("Игорь".into()),
                    last_name: Some("Герасимов".into()),
                    screen_name: Some("igor".into()),
                },
            },
        ]
    }

    #[test]
    fn from_config_derives_initial_state() {
        assert_eq!(
            LoginFlow::from_config(&VkConfig::default()).kind(),
            "not_configured"
        );
        let with_token = VkConfig {
            access_token: SecretToken::new("vk1.a.x"),
            ..VkConfig::default()
        };
        assert_eq!(LoginFlow::from_config(&with_token).kind(), "disconnected");
        // Пробельный токен — это отсутствие токена, а не «настроено».
        let blank = VkConfig {
            access_token: SecretToken::new("   "),
            ..VkConfig::default()
        };
        assert_eq!(LoginFlow::from_config(&blank).kind(), "not_configured");
    }

    /// Словарь `state` — контракт с фронтом: `js/vk/login.js` рисует экран по
    /// строке, и переименование обязано валить тест, а не тихо ломать вкладку.
    #[test]
    fn kinds_cover_the_documented_dictionary() {
        let kinds: Vec<&str> = all_states().iter().map(LoginFlow::kind).collect();
        assert!(kinds.contains(&"not_configured"));
        assert!(kinds.contains(&"checking"));
        assert!(kinds.contains(&"disconnected"));
        assert!(kinds.contains(&"invalid_token"));
        assert!(kinds.contains(&"no_messages_scope"));
        assert!(kinds.contains(&"captcha_required"));
        assert!(kinds.contains(&"authorized"));
        // Утверждённое имя состояния — `no_messages_scope`. Вариант
        // `insufficient_scope` из чернового фронт-плана отвергнут (Ф1.1):
        // то же имя используется как `code` в конверте ошибки для VK-15.
        assert!(!kinds.contains(&"insufficient_scope"));
    }

    #[test]
    fn dto_carries_only_the_fields_of_its_state() {
        let dto = LoginFlow::NotConfigured.to_dto();
        assert_eq!(dto.state, "not_configured");
        assert!(dto.error.is_none() && dto.user.is_none());

        let dto = LoginFlow::Checking.to_dto();
        assert!(dto.error.is_none() && dto.captcha_sid.is_none());

        let dto = LoginFlow::InvalidToken {
            error: "boom".into(),
        }
        .to_dto();
        assert_eq!(dto.error.as_deref(), Some("boom"));
        assert!(dto.user.is_none());

        let dto = LoginFlow::CaptchaRequired {
            sid: "1".into(),
            img: "u".into(),
        }
        .to_dto();
        assert_eq!(dto.captcha_sid.as_deref(), Some("1"));
        assert_eq!(dto.captcha_img.as_deref(), Some("u"));
        assert!(dto.user.is_none());

        let dto = LoginFlow::Authorized {
            user: VkUserDto {
                id: "7".into(),
                ..VkUserDto::default()
            },
        }
        .to_dto();
        assert_eq!(dto.user.unwrap().id, "7");
        assert!(dto.error.is_none());
    }

    #[test]
    fn authorized_and_configured_predicates() {
        for flow in all_states() {
            let kind = flow.kind();
            assert_eq!(flow.is_authorized(), kind == "authorized", "{kind}");
            assert_eq!(flow.is_configured(), kind != "not_configured", "{kind}");
        }
    }

    /// Инвариант утечки: ни одно достижимое состояние не кладёт в JSON
    /// подстроку токена. Здесь это проверяется «в лоб» — токен физически не
    /// может попасть в DTO по типам, но тест фиксирует намерение и сломается,
    /// если кто-то добавит поле `token: String`.
    #[test]
    fn login_state_never_leaks_the_token() {
        const TOKEN: &str = "vk1.a.SUPERSECRETVALUE";
        for flow in all_states() {
            let json = serde_json::to_string(&flow.to_dto()).unwrap();
            assert!(
                !json.contains(TOKEN) && !json.contains("SUPERSECRET"),
                "{}: {json}",
                flow.kind()
            );
        }
    }

    #[test]
    fn dto_round_trips() {
        for flow in all_states() {
            let dto = flow.to_dto();
            let json = serde_json::to_string(&dto).unwrap();
            let back: LoginStateDto = serde_json::from_str(&json).unwrap();
            assert_eq!(dto, back, "round-trip сломался на {json}");
        }
    }

    // ------------------------------------------------------------------
    // Ф3.1 — разбор вставленного redirect-URL.
    // ------------------------------------------------------------------

    /// Токен-канарейка: достаточно длинный, чтобы пройти [`MIN_TOKEN_LEN`], и
    /// узнаваемый в любом дампе.
    const TOKEN: &str = "vk1.a.SECRET-CANARY_0123456789abcdef";

    /// ГЛАВНАЯ таблица фазы: три принимаемые формы + мусор.
    ///
    /// Каждая строка — реальный вид буфера обмена, а не выдуманный вход:
    /// пользователь копирует адресную строку целиком, иногда с переносами
    /// (терминал, письмо), иногда — только хвост, иногда — сам токен.
    #[test]
    fn parse_token_input_table() {
        // (вход, ожидаемый user_id, ожидаемый expires_in)
        let ok: Vec<(String, Option<i64>, Option<u64>)> = vec![
            // 1. Полный URL с фрагментом — ровно то, что даёт Implicit Flow.
            (
                format!("https://oauth.vk.ru/blank.html#access_token={TOKEN}&expires_in=0&user_id=105431859&email=user%40example.com"),
                Some(105_431_859),
                Some(0),
            ),
            // 2. Голый фрагмент без URL.
            (
                format!("access_token={TOKEN}&expires_in=86400&user_id=7"),
                Some(7),
                Some(86_400),
            ),
            // 3. Просто токен.
            (TOKEN.to_string(), None, None),
            // 4. Порядок параметров вперемешку.
            (
                format!("https://oauth.vk.ru/blank.html#user_id=42&email=a%40b.c&expires_in=0&access_token={TOKEN}"),
                Some(42),
                Some(0),
            ),
            // 5. Лишние (в том числе будущие) параметры.
            (
                format!("#state=xyz&access_token={TOKEN}&token_type=bearer&scope=messages,offline&user_id=1"),
                Some(1),
                None,
            ),
            // 6. Копирование из терминала: перенос строки и пробелы ВНУТРИ.
            (
                format!("https://oauth.vk.ru/blank.html#access_to\n  ken={TOKEN}\n&user_id=9\n"),
                Some(9),
                None,
            ),
            // 7. URL с `?` вместо `#`.
            (
                format!("https://oauth.vk.ru/blank.html?access_token={TOKEN}&user_id=3"),
                Some(3),
                None,
            ),
            // 8. Фрагмент выигрывает у query — токен лежит после `#`.
            (
                format!("https://oauth.vk.ru/blank.html?foo=1#access_token={TOKEN}"),
                None,
                None,
            ),
            // 9. Мусорные значения подсказок не роняют разбор: без токена
            //    жить нельзя, без user_id — можно.
            (
                format!("#access_token={TOKEN}&user_id=abc&expires_in=-1"),
                None,
                None,
            ),
        ];

        for (input, user_id, expires_in) in ok {
            let parsed = parse_token_input(&input)
                .unwrap_or_else(|e| panic!("разбор обязан пройти: {input:?} → {e}"));
            assert_eq!(parsed.token.expose(), TOKEN, "вход {input:?}");
            assert_eq!(parsed.user_id, user_id, "user_id для {input:?}");
            assert_eq!(parsed.expires_in, expires_in, "expires_in для {input:?}");
        }

        let bad = [
            "",
            "   \n\t ",
            // `#` есть, `access_token` нет.
            "https://oauth.vk.ru/blank.html#error=access_denied&error_reason=user_denied",
            // URL без фрагмента вовсе: `:` и `/` в токене не встречаются.
            "https://oauth.vk.ru/blank.html",
            // Обрывок копирования.
            "vk1.a.",
            "abc",
            // Пустое значение параметра.
            "#access_token=&user_id=1",
            // Символы, которых в токене не бывает.
            "#access_token=vk1.a.полтокена+ещё",
        ];
        for input in bad {
            let err = parse_token_input(input)
                .err()
                .unwrap_or_else(|| panic!("вход {input:?} обязан быть отвергнут"));
            assert!(
                matches!(err, LoginError::BadInput { .. }),
                "{input:?} → {err:?}"
            );
            // Текст отказа не цитирует ввод: он и есть секрет.
            assert!(
                !err.to_string().contains("vk1.a."),
                "ввод процитирован в ошибке: {err}"
            );
        }
    }

    /// `email` не должен доехать НИКУДА: ни в структуру (поля нет), ни в
    /// токен, ни в отладочный вывод. Мутация «сохранить email про запас»
    /// обязана красить этот тест.
    #[test]
    fn parse_token_input_drops_the_email() {
        const EMAIL: &str = "victim%40example.com";
        let parsed = parse_token_input(&format!(
            "https://oauth.vk.ru/blank.html#access_token={TOKEN}&email={EMAIL}&user_id=1"
        ))
        .unwrap();

        assert!(!parsed.token.expose().contains("example.com"));
        let dumped = format!("{parsed:?}");
        assert!(!dumped.contains("example.com"), "email в Debug: {dumped}");
        assert!(!dumped.contains("victim"), "email в Debug: {dumped}");
        // И сам токен в Debug не светится — за это отвечает SecretToken.
        assert!(!dumped.contains("SECRET-CANARY"), "{dumped}");
    }

    /// `expires_in = 0` — бессрочный токен (scope `offline`); ненулевой даёт
    /// момент протухания, о котором `/status` предупредит ЗАРАНЕЕ, а не по
    /// факту кода 5 в неудобный момент.
    #[test]
    fn expiry_is_derived_from_expires_in() {
        let now = 1_800_000_000;

        let permanent = parse_token_input(&format!("#access_token={TOKEN}&expires_in=0")).unwrap();
        assert!(permanent.is_permanent());
        assert_eq!(permanent.expires_at(now), None);

        let bare = parse_token_input(TOKEN).unwrap();
        assert!(bare.is_permanent(), "без фрагмента срок неизвестен ⇒ бессрочный");

        let temporary =
            parse_token_input(&format!("#access_token={TOKEN}&expires_in=86400")).unwrap();
        assert!(!temporary.is_permanent());
        assert_eq!(temporary.expires_at(now), Some(now + 86_400));

        // Переполнение не паникует: u64::MAX секунд «протухнут» в i64::MAX.
        let absurd =
            parse_token_input(&format!("#access_token={TOKEN}&expires_in={}", u64::MAX)).unwrap();
        assert_eq!(absurd.expires_at(now), Some(i64::MAX));
    }

    /// Разбор ЧИСТЫЙ: ни одной строки в лог, что бы ни подали на вход.
    ///
    /// Мутация: дописать `tracing::warn!(raw)` в `parse_token_input` — тест
    /// обязан покраснеть.
    #[test]
    fn parse_token_input_writes_nothing_to_the_log() {
        let mark = crate::testlog::mark();
        for input in [
            format!("https://oauth.vk.ru/blank.html#access_token={TOKEN}&email=a%40b.c"),
            TOKEN.to_string(),
            "мусор без токена".to_string(),
            String::new(),
        ] {
            let _ = parse_token_input(&input);
        }
        let logged = crate::testlog::since(mark);
        assert!(
            logged.is_empty(),
            "parse_token_input обязан молчать, а написал:\n{logged}"
        );
    }

    // ------------------------------------------------------------------
    // Ф3.2 — переходы автомата на фейковом бэкенде (без сети).
    // ------------------------------------------------------------------

    /// Бэкенд со сценарием: что вернуть на `whoami` и что на
    /// `probe_messages`, плюс журнал вызовов.
    struct FakeBackend {
        whoami: Result<VkUserDto, LoginError>,
        probe: Result<(), LoginError>,
        calls: std::sync::Mutex<Vec<String>>,
        seen_captcha: std::sync::Mutex<Vec<Option<Captcha>>>,
    }

    impl FakeBackend {
        fn new(whoami: Result<VkUserDto, LoginError>, probe: Result<(), LoginError>) -> Self {
            Self {
                whoami,
                probe,
                calls: std::sync::Mutex::new(Vec::new()),
                seen_captcha: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn ok() -> Self {
            Self::new(
                Ok(VkUserDto {
                    id: "105431859".into(),
                    first_name: Some("Игорь".into()),
                    ..VkUserDto::default()
                }),
                Ok(()),
            )
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LoginBackend for FakeBackend {
        async fn whoami(&self, captcha: Option<&Captcha>) -> Result<VkUserDto, LoginError> {
            self.calls.lock().unwrap().push("whoami".into());
            self.seen_captcha.lock().unwrap().push(captcha.cloned());
            self.whoami.clone()
        }
        async fn probe_messages(&self, captcha: Option<&Captcha>) -> Result<(), LoginError> {
            self.calls.lock().unwrap().push("probe".into());
            self.seen_captcha.lock().unwrap().push(captcha.cloned());
            self.probe.clone()
        }
    }

    fn state_with_token() -> Arc<VkState> {
        let dir = std::env::temp_dir().join("forge-vk-login-tests");
        VkState::new(VkConfig {
            access_token: SecretToken::new(TOKEN),
            cache_dir: dir,
            ..VkConfig::default()
        })
    }

    /// По тесту на каждый переход: успех, 5, 15, 14, транспорт.
    #[tokio::test]
    async fn verify_token_transition_table() {
        // Успех.
        let st = state_with_token();
        let backend = FakeBackend::ok();
        let flow = verify_token(&st, &backend, None).await;
        assert_eq!(flow.kind(), "authorized");
        assert_eq!(st.login_state_dto().await.state, "authorized");
        assert_eq!(
            backend.calls(),
            vec!["whoami", "probe"],
            "порядок проверок фиксирован: users.get, потом messages.*"
        );

        // Код 5 на whoami — до пробы дело не доходит.
        let st = state_with_token();
        let backend = FakeBackend::new(
            Err(LoginError::InvalidToken {
                message: "VK error 5".into(),
            }),
            Ok(()),
        );
        assert_eq!(verify_token(&st, &backend, None).await.kind(), "invalid_token");
        assert_eq!(
            backend.calls(),
            vec!["whoami"],
            "протухший токен не должен доходить до пробы scope"
        );

        // Код 15 виден только на пробе — ради неё она и существует.
        let st = state_with_token();
        let backend = FakeBackend::new(
            Ok(VkUserDto::default()),
            Err(LoginError::NoMessagesScope {
                message: "VK error 15".into(),
            }),
        );
        let flow = verify_token(&st, &backend, None).await;
        assert_eq!(flow.kind(), "no_messages_scope");
        assert_eq!(backend.calls(), vec!["whoami", "probe"]);

        // Код 14 — captcha; адрес картинки обязан осесть для прокси.
        let st = state_with_token();
        let backend = FakeBackend::new(
            Err(LoginError::Captcha {
                sid: "239123".into(),
                img: "https://api.vk.com/captcha.php?sid=239123".into(),
            }),
            Ok(()),
        );
        let flow = verify_token(&st, &backend, None).await;
        assert_eq!(flow.kind(), "captcha_required");
        assert_eq!(
            st.captcha_img("239123").await.as_deref(),
            Some("https://api.vk.com/captcha.php?sid=239123"),
            "прокси картинки берёт URL только отсюда"
        );

        // Транспорт — лечится повтором, отдельного экрана не заводим.
        let st = state_with_token();
        let backend = FakeBackend::new(
            Err(LoginError::Transport {
                message: "connection reset".into(),
            }),
            Ok(()),
        );
        let flow = verify_token(&st, &backend, None).await;
        assert_eq!(flow.kind(), "disconnected");
        assert_eq!(
            flow.to_dto().error.as_deref(),
            Some("connection reset"),
            "текст причины обязан доехать до пользователя"
        );
    }

    /// Без токена сеть не трогаем вовсе: `spawn_workers` зовёт проверку на
    /// каждом старте, и запрос «расскажи о владельце пустого токена» был бы
    /// гарантированным кодом 5 в логе на ровном месте.
    #[tokio::test]
    async fn verify_token_without_a_token_never_calls_the_backend() {
        let st = VkState::new(VkConfig {
            cache_dir: std::env::temp_dir().join("forge-vk-login-tests"),
            ..VkConfig::default()
        });
        let backend = FakeBackend::ok();
        assert_eq!(verify_token(&st, &backend, None).await.kind(), "not_configured");
        assert!(backend.calls().is_empty());
    }

    /// Перед сетевым вызовом автомат обязан пройти через `checking`: без него
    /// UI мигал бы «отключено» на каждом старте процесса.
    #[tokio::test]
    async fn verify_token_announces_checking_before_the_network() {
        let st = state_with_token();
        let mut rx = st.subscribe();
        let backend = FakeBackend::ok();
        verify_token(&st, &backend, None).await;

        let mut states = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let VkServerMsg::LoginState(dto) = ev {
                states.push(dto.state);
            }
        }
        assert_eq!(
            states,
            vec!["checking".to_string(), "authorized".to_string()],
            "порядок кадров login_state"
        );
    }

    /// Ответ на captcha передаётся В ОБА вызова как есть: повтор проверки —
    /// это тот же запрос плюс два поля, а не новый сценарий.
    #[tokio::test]
    async fn captcha_answer_reaches_both_probes() {
        let st = state_with_token();
        let backend = FakeBackend::ok();
        let captcha = Captcha {
            sid: "239".into(),
            key: "abcde".into(),
        };
        verify_token(&st, &backend, Some(&captcha)).await;
        let seen = backend.seen_captcha.lock().unwrap().clone();
        assert_eq!(seen, vec![Some(captcha.clone()), Some(captcha)]);
    }

    /// Точечные переходы, которые зовут data-роуты Ф4+ прямо из обработчика
    /// ошибки: состояние обязано меняться там же, а не ждать ручной проверки.
    #[tokio::test]
    async fn targeted_transitions_broadcast_their_state() {
        let st = state_with_token();
        assert_eq!(
            on_auth_invalidated(&st, "token revoked").await.kind(),
            "invalid_token"
        );
        assert_eq!(on_scope_lost(&st, "no messages").await.kind(), "no_messages_scope");
        assert_eq!(
            on_captcha(&st, "1".into(), "https://vk/c.php".into())
                .await
                .kind(),
            "captcha_required"
        );
        assert_eq!(st.login_state_dto().await.captcha_sid.as_deref(), Some("1"));
    }

    /// Таблица «ошибка VK → отказ логина». Ловит перепутанные коды: 5 и 15
    /// рисуют РАЗНЫЕ экраны, и обмен местами здесь стоил бы пользователю
    /// часа вставки одного и того же токена по кругу.
    #[test]
    fn vk_errors_map_to_login_errors() {
        let api = |code: i64| VkError::Api {
            code,
            msg: "текст VK".into(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        assert!(matches!(
            LoginError::from(&api(5)),
            LoginError::InvalidToken { .. }
        ));
        assert!(matches!(
            LoginError::from(&api(15)),
            LoginError::NoMessagesScope { .. }
        ));
        assert!(matches!(LoginError::from(&api(100)), LoginError::Vk { .. }));
        assert!(matches!(
            LoginError::from(&VkError::Transport {
                reason: "reset".into()
            }),
            LoginError::Transport { .. }
        ));
        assert!(matches!(
            LoginError::from(&VkError::Http { status: 502 }),
            LoginError::Transport { .. }
        ));
        assert!(matches!(
            LoginError::from(&VkError::Decode {
                reason: "eof".into()
            }),
            LoginError::Vk { .. }
        ));

        // Код 14 с обоими полями — captcha; без них показывать нечего.
        let full = VkError::Api {
            code: 14,
            msg: String::new(),
            captcha_sid: Some("239".into()),
            captcha_img: Some("https://vk/c.php".into()),
            redirect_uri: None,
        };
        assert_eq!(
            LoginError::from(&full),
            LoginError::Captcha {
                sid: "239".into(),
                img: "https://vk/c.php".into()
            }
        );
        assert!(matches!(LoginError::from(&api(14)), LoginError::Vk { .. }));

        // Дословный текст VK доезжает до пользователя вместе с каноническим.
        let text = LoginError::from(&api(5)).to_string();
        assert!(text.contains("текст VK"), "{text}");
        assert!(text.contains("VK error 5"), "{text}");
    }

    /// Профиль VK → DTO: пустые строки становятся `None`, чтобы
    /// `display_name` не показывал «  » вместо имени.
    #[test]
    fn user_dto_is_built_from_the_raw_profile() {
        let raw = ProfileRaw {
            id: 105_431_859,
            first_name: "Игорь".into(),
            last_name: " ".into(),
            screen_name: "igor".into(),
            ..ProfileRaw::default()
        };
        let dto = VkUserDto::from(&raw);
        assert_eq!(dto.id, "105431859");
        assert_eq!(dto.first_name.as_deref(), Some("Игорь"));
        assert_eq!(dto.last_name, None, "пробельная фамилия — это её отсутствие");
        assert_eq!(dto.display_name(), "Игорь");
    }

    #[test]
    fn display_name_falls_back_predictably() {
        let full = VkUserDto {
            id: "1".into(),
            first_name: Some("Иван".into()),
            last_name: Some("Петров".into()),
            screen_name: Some("ivan".into()),
        };
        assert_eq!(full.display_name(), "Иван Петров");

        let only_screen = VkUserDto {
            id: "2".into(),
            first_name: Some("  ".into()),
            last_name: None,
            screen_name: Some("nick".into()),
        };
        assert_eq!(only_screen.display_name(), "@nick");

        let bare = VkUserDto {
            id: "3".into(),
            ..VkUserDto::default()
        };
        assert_eq!(bare.display_name(), "3");
    }
}
