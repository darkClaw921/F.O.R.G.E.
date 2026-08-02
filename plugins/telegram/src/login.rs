//! Конечный автомат авторизации личного аккаунта Telegram.
//!
//! ```text
//!   NotConfigured ──(save_credentials)──▶ Disconnected
//!   Disconnected  ──(login/start)──────▶ CodeSent
//!   CodeSent      ──(login/code)───────▶ Authorized | PasswordRequired
//!   PasswordRequired ─(login/password)─▶ Authorized
//!   *             ──(logout / AUTH_KEY_UNREGISTERED)──▶ Disconnected
//! ```
//!
//! # Как это устроено
//!
//! Модуль разделён на две половины:
//!
//! - **чистая** — [`LoginFlow`] и функции-переходы [`request_code`],
//!   [`submit_code`], [`submit_password`], [`sign_out`],
//!   [`on_auth_invalidated`]. Они работают через трейт [`LoginBackend`] и
//!   ничего не знают о сети, поэтому матрица переходов покрывается
//!   обычными unit-тестами;
//! - **сетевая** — [`MtBackend`], тонкая обёртка над `grammers_client::Client`,
//!   которая переводит его типы и ошибки в термины этого модуля.
//!
//! Каждый успешный переход идёт через [`TelegramState::set_login`], а
//! значит рассылает `login_state` всем WS-подписчикам под тем же lock'ом,
//! что и запись состояния.
//!
//! # Токены живут только в памяти
//!
//! [`LoginToken`] / [`PasswordToken`] — эфемерные объекты MTProto-сессии
//! логина. Они НЕ персистятся: рестарт процесса посреди ввода кода
//! означает «начать логин заново» (пользователь просто нажимает
//! «отправить код» ещё раз). Это осознанный размен: хранить на диске
//! наполовину завершённый логин опаснее, чем переспросить.
//!
//! # Грабли grammers 0.10, ради которых токены — `Option`
//!
//! `Client::sign_in` берёт `&LoginToken` и НЕ расходует его: после
//! неверного кода тот же токен годится для повторной попытки, поэтому
//! состояние `CodeSent` остаётся нетронутым.
//!
//! `Client::check_password` ПОГЛОЩАЕТ `PasswordToken`, а при неверном
//! пароле возвращает НОВЫЙ токен внутри `SignInError::InvalidPassword`.
//! Его обязательно нужно положить обратно в `PasswordRequired`, иначе
//! одна опечатка заставляла бы проходить логин с самого начала. Если же
//! `check_password` упал по другой причине (FLOOD_WAIT, обрыв связи),
//! токен потерян безвозвратно — тогда в состоянии остаётся
//! `PasswordRequired{token: None}`, и UI обязан начать логин заново.
//!
//! # Токены никогда не покидают процесс
//!
//! Наружу отдаётся только [`LoginStateDto`] — проекция без токенов
//! (см. [`LoginFlow::to_dto`] и тест `dto_never_leaks_tokens`).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use grammers_client::client::{
    LoginToken as MtLoginToken, PasswordToken as MtPasswordToken, SignInError,
};
use grammers_client::Client;
use grammers_mtsender::InvocationError;
use serde::{Deserialize, Serialize};

use crate::config::TelegramConfig;
use crate::runtime::flood_wait_secs;
use crate::state::TelegramState;

/// Непрозрачный токен «код отправлен» (ответ `auth.sendCode`).
///
/// Обёртка над токеном grammers: наружу не сериализуется, в `Debug`
/// печатается как `<redacted>`. Он даёт право завершить вход в аккаунт,
/// поэтому обращаться с ним нужно как с секретом.
pub struct LoginToken(LoginTokenInner);

enum LoginTokenInner {
    /// Настоящий токен MTProto.
    Mt(Box<MtLoginToken>),
    /// Заглушка для unit-тестов матрицы переходов: конструктор
    /// `MtLoginToken` приватен в grammers, поэтому фейковый бэкенд не
    /// может создать настоящий токен.
    #[cfg(test)]
    Stub(String),
}

impl LoginToken {
    /// Оборачивает токен, полученный от grammers.
    pub fn from_mt(token: MtLoginToken) -> Self {
        Self(LoginTokenInner::Mt(Box::new(token)))
    }

    /// Настоящий токен, если он есть (у тестовой заглушки — `None`).
    pub fn as_mt(&self) -> Option<&MtLoginToken> {
        match &self.0 {
            LoginTokenInner::Mt(t) => Some(t),
            #[cfg(test)]
            LoginTokenInner::Stub(_) => None,
        }
    }

    /// Тестовая заглушка токена.
    #[cfg(test)]
    pub fn stub(tag: &str) -> Self {
        Self(LoginTokenInner::Stub(tag.to_string()))
    }

    /// Метка тестовой заглушки — по ней тесты отличают «тот же токен» от
    /// «новый токен, пришедший вместе с ошибкой».
    #[cfg(test)]
    pub fn stub_tag(&self) -> Option<&str> {
        match &self.0 {
            LoginTokenInner::Mt(_) => None,
            LoginTokenInner::Stub(s) => Some(s),
        }
    }
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoginToken(<redacted>)")
    }
}

/// Непрозрачный токен «нужен пароль 2FA» (ответ `auth.signIn` с
/// `SESSION_PASSWORD_NEEDED`).
///
/// `Option` в [`LoginFlow::PasswordRequired`] не случаен — см. «Грабли
/// grammers 0.10» в шапке модуля.
pub struct PasswordToken {
    inner: PasswordTokenInner,
    hint: Option<String>,
}

enum PasswordTokenInner {
    /// Настоящий токен MTProto (расходуется `check_password`).
    Mt(Box<MtPasswordToken>),
    /// Заглушка для unit-тестов.
    #[cfg(test)]
    Stub(String),
}

impl PasswordToken {
    /// Оборачивает токен grammers, запоминая подсказку к паролю.
    pub fn from_mt(token: MtPasswordToken) -> Self {
        let hint = token.hint().map(str::to_string);
        Self {
            inner: PasswordTokenInner::Mt(Box::new(token)),
            hint,
        }
    }

    /// Подсказка к паролю, заданная пользователем в Telegram.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// Разворачивает настоящий токен для передачи в `check_password`.
    ///
    /// Возвращает `None` для тестовой заглушки.
    pub fn into_mt(self) -> Option<MtPasswordToken> {
        match self.inner {
            PasswordTokenInner::Mt(t) => Some(*t),
            #[cfg(test)]
            PasswordTokenInner::Stub(_) => None,
        }
    }

    /// Тестовая заглушка токена с заданной подсказкой.
    #[cfg(test)]
    pub fn stub(tag: &str, hint: Option<&str>) -> Self {
        Self {
            inner: PasswordTokenInner::Stub(tag.to_string()),
            hint: hint.map(str::to_string),
        }
    }

    /// Метка тестовой заглушки (см. [`LoginToken::stub_tag`]).
    #[cfg(test)]
    pub fn stub_tag(&self) -> Option<&str> {
        match &self.inner {
            PasswordTokenInner::Mt(_) => None,
            PasswordTokenInner::Stub(s) => Some(s),
        }
    }
}

impl fmt::Debug for PasswordToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordToken")
            .field("hint", &self.hint)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Пользователь Telegram в проекции для UI.
///
/// `id` — строка, потому что Telegram-идентификаторы 64-битные, а
/// JS-число теряет точность после 2^53.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TgUserDto {
    /// Числовой id аккаунта строкой.
    pub id: String,
    /// `@username`, если задан.
    pub username: Option<String>,
    /// Имя (first name).
    pub first_name: Option<String>,
    /// Телефон, если аккаунт его отдаёт.
    pub phone: Option<String>,
}

impl TgUserDto {
    /// Проекция пользователя grammers в DTO.
    pub fn from_user(user: &grammers_client::peer::User) -> Self {
        Self {
            id: user.id().to_string(),
            username: user.username().map(str::to_string),
            first_name: user.first_name().map(str::to_string),
            phone: user.phone().map(str::to_string),
        }
    }
}

/// Сериализуемая проекция [`LoginFlow`] — единственное, что уходит
/// клиенту (REST `status` и WS `login_state`).
///
/// Поле `state` совпадает с [`LoginFlow::kind`] и является контрактом
/// для фронта: `not_configured | disconnected | code_sent |
/// password_required | authorized`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStateDto {
    /// Машинное имя состояния (см. [`LoginFlow::kind`]).
    pub state: String,
    /// Телефон, на который отправлен код (только в `code_sent`).
    pub phone: Option<String>,
    /// Подсказка к паролю 2FA (только в `password_required`).
    pub hint: Option<String>,
    /// Текст последней ошибки (только в `disconnected`).
    pub error: Option<String>,
    /// Авторизованный пользователь (только в `authorized`).
    pub user: Option<TgUserDto>,
}

/// Состояние авторизации плагина.
///
/// Живёт под `Mutex` в [`crate::state::TelegramState`]; каждый переход
/// broadcast'ит `login_state` всем WS-подписчикам, чтобы открытые вкладки
/// показывали одинаковую картину.
///
/// НЕ `Clone`: токены внутри неклонируемы by design — существовать должен
/// ровно один экземпляр каждого токена, иначе легко «продолжить логин»
/// устаревшей копией.
#[derive(Debug)]
pub enum LoginFlow {
    /// Нет `api_id`/`api_hash` — логин недоступен, UI показывает первый
    /// шаг wizard'а (ввод кредов с my.telegram.org).
    NotConfigured,
    /// Креды есть, но авторизованного клиента нет: ещё не входили, вышли,
    /// сессию отозвали с другого устройства, либо соединение упало.
    /// `error` — человекочитаемая причина последнего сбоя, если была.
    Disconnected {
        /// Причина последнего сбоя (если состояние наступило из-за ошибки).
        error: Option<String>,
    },
    /// Код подтверждения отправлен, ждём его ввода.
    CodeSent {
        /// Токен `auth.sendCode` — наружу не отдаётся.
        ///
        /// `None` на время выполнения `sign_in`: токен физически изъят из
        /// состояния, чтобы не держать `Mutex` во время сетевого вызова
        /// (иначе `GET /status` и WS-подключения вставали бы на всю
        /// длительность RPC). Параллельный `login/code` в этот момент
        /// честно получает `invalid_state`, а не гонку.
        token: Option<LoginToken>,
        /// Телефон, на который ушёл код (для отображения в UI).
        phone: String,
    },
    /// Нужен пароль 2FA (cloud password).
    PasswordRequired {
        /// Токен продолжения; `None` ⇒ токен утерян, логин с начала.
        token: Option<PasswordToken>,
        /// Подсказка к паролю, заданная пользователем в Telegram.
        hint: Option<String>,
    },
    /// Успешная авторизация.
    Authorized {
        /// Данные вошедшего аккаунта.
        user: TgUserDto,
    },
}

impl LoginFlow {
    /// Выводит начальное состояние из конфига: оба креда на месте ⇒
    /// [`LoginFlow::Disconnected`], иначе [`LoginFlow::NotConfigured`].
    pub fn from_config(cfg: &TelegramConfig) -> Self {
        if cfg.has_credentials() {
            Self::Disconnected { error: None }
        } else {
            Self::NotConfigured
        }
    }

    /// Машинное имя состояния — контракт REST/WS.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Disconnected { .. } => "disconnected",
            Self::CodeSent { .. } => "code_sent",
            Self::PasswordRequired { .. } => "password_required",
            Self::Authorized { .. } => "authorized",
        }
    }

    /// Авторизован ли плагин прямо сейчас.
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized { .. })
    }

    /// Настроены ли креды приложения (любое состояние кроме
    /// [`LoginFlow::NotConfigured`]).
    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::NotConfigured)
    }

    /// Проекция для клиента. Токены НИКОГДА не сериализуются.
    pub fn to_dto(&self) -> LoginStateDto {
        let mut dto = LoginStateDto {
            state: self.kind().to_string(),
            phone: None,
            hint: None,
            error: None,
            user: None,
        };
        match self {
            Self::NotConfigured => {}
            Self::Disconnected { error } => dto.error = error.clone(),
            Self::CodeSent { phone, .. } => dto.phone = Some(phone.clone()),
            Self::PasswordRequired { hint, .. } => dto.hint = hint.clone(),
            Self::Authorized { user } => dto.user = Some(user.clone()),
        }
        dto
    }
}

/// Ошибка шага логина в терминах REST-контракта плагина.
///
/// Маппинг в HTTP делает `impl From<LoginError> for ApiError`
/// (`routes/error.rs`), чтобы хендлеры не собирали ответы руками.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// 409 `invalid_state` — шаг невозможен из текущего состояния.
    InvalidState(String),
    /// 400 `invalid_input` — Telegram отверг введённые данные
    /// (`PHONE_CODE_INVALID`, `PASSWORD_HASH_INVALID`, `PHONE_NUMBER_INVALID`).
    /// Пользователь может исправить ввод и повторить.
    BadInput(String),
    /// 429 `flood_wait` — Telegram просит подождать. НИКОГДА не
    /// ретраится автоматически (риск №2 плана: FLOOD_WAIT на логине
    /// измеряется часами, авто-повторы только продлевают блокировку).
    FloodWait {
        /// Сколько секунд ждать.
        secs: u64,
        /// Готовое сообщение для UI.
        message: String,
    },
    /// 409 `invalid_state` — аккаунта не существует; регистрация из
    /// стороннего клиента невозможна (ограничение Telegram).
    SignUpRequired,
    /// 503 `not_connected` — сеть/MTProto недоступны.
    NotConnected(String),
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(m) | Self::BadInput(m) | Self::NotConnected(m) => f.write_str(m),
            Self::FloodWait { message, .. } => f.write_str(message),
            Self::SignUpRequired => f.write_str(
                "this phone number has no Telegram account yet: sign-up is only possible \
                 from an official Telegram app",
            ),
        }
    }
}

impl std::error::Error for LoginError {}

impl LoginError {
    /// Классифицирует ошибку RPC grammers.
    pub fn from_invocation(err: &InvocationError) -> Self {
        if let Some(secs) = flood_wait_secs(err) {
            return Self::flood(u64::from(secs));
        }
        match err {
            InvocationError::Rpc(rpc) if rpc.code == 400 => {
                Self::BadInput(format!("Telegram rejected the request: {}", rpc.name))
            }
            InvocationError::Rpc(rpc) if rpc.code == 401 => Self::InvalidState(format!(
                "Telegram authorization is no longer valid ({}): sign in again",
                rpc.name
            )),
            other => Self::NotConnected(format!("Telegram request failed: {other}")),
        }
    }

    /// `429` с готовым текстом.
    pub fn flood(secs: u64) -> Self {
        Self::FloodWait {
            secs,
            message: format!(
                "Telegram asks to wait {secs}s before trying again (FLOOD_WAIT). \
                 Do not retry automatically — repeated attempts extend the ban."
            ),
        }
    }
}

/// Результат шага «ввод кода».
#[derive(Debug)]
pub enum SignInStep {
    /// Вход завершён.
    Authorized(TgUserDto),
    /// У аккаунта включена 2FA — нужен пароль.
    PasswordRequired(PasswordToken),
}

/// Результат шага «ввод пароля 2FA».
#[derive(Debug)]
pub enum PasswordStep {
    /// Вход завершён.
    Authorized(TgUserDto),
    /// Пароль неверный; Telegram вернул НОВЫЙ токен — его кладут обратно
    /// в состояние, чтобы пользователь мог повторить ввод.
    Invalid(PasswordToken),
}

/// Сетевая часть логина.
///
/// Существует ради тестируемости: переходы автомата гоняются на фейковой
/// реализации без сети, а продакшен использует [`MtBackend`].
#[async_trait]
pub trait LoginBackend: Send + Sync {
    /// `auth.sendCode` — отправить код на телефон.
    async fn request_login_code(&self, phone: &str) -> Result<LoginToken, LoginError>;

    /// `auth.signIn` — токен НЕ расходуется, поэтому берётся по ссылке.
    async fn sign_in(&self, token: &LoginToken, code: &str) -> Result<SignInStep, LoginError>;

    /// `auth.checkPassword` — токен РАСХОДУЕТСЯ (см. шапку модуля).
    async fn check_password(
        &self,
        token: PasswordToken,
        password: &str,
    ) -> Result<PasswordStep, LoginError>;

    /// `auth.logOut`.
    async fn sign_out(&self) -> Result<(), LoginError>;

    /// Данные вошедшего аккаунта (`users.getUsers[InputUserSelf]`).
    async fn get_me(&self) -> Result<TgUserDto, LoginError>;
}

/// Реализация [`LoginBackend`] поверх живого `grammers_client::Client`.
pub struct MtBackend {
    client: Client,
    api_hash: String,
}

impl MtBackend {
    /// `api_hash` нужен отдельно: `Client` хранит только `api_id`, а
    /// `auth.sendCode` требует оба креда.
    pub fn new(client: Client, api_hash: impl Into<String>) -> Self {
        Self {
            client,
            api_hash: api_hash.into(),
        }
    }

    /// Классифицирует ошибку входа grammers, у которой нет отдельного
    /// варианта в [`SignInStep`] / [`PasswordStep`].
    fn map_sign_in_error(err: SignInError) -> LoginError {
        match err {
            SignInError::SignUpRequired => LoginError::SignUpRequired,
            SignInError::InvalidCode => LoginError::BadInput(
                "the confirmation code is invalid — check the digits and try again".into(),
            ),
            SignInError::InvalidPassword(_) => LoginError::BadInput(
                "the 2FA password is invalid — check the password and try again".into(),
            ),
            SignInError::PasswordRequired(_) => {
                LoginError::InvalidState("this account requires a 2FA password".into())
            }
            SignInError::Other(e) => LoginError::from_invocation(&e),
        }
    }
}

#[async_trait]
impl LoginBackend for MtBackend {
    async fn request_login_code(&self, phone: &str) -> Result<LoginToken, LoginError> {
        self.client
            .request_login_code(phone, &self.api_hash)
            .await
            .map(LoginToken::from_mt)
            .map_err(|e| LoginError::from_invocation(&e))
    }

    async fn sign_in(&self, token: &LoginToken, code: &str) -> Result<SignInStep, LoginError> {
        let mt = token.as_mt().ok_or_else(|| {
            LoginError::InvalidState("login token is unusable — restart the login".into())
        })?;
        match self.client.sign_in(mt, code).await {
            Ok(user) => Ok(SignInStep::Authorized(TgUserDto::from_user(&user))),
            Err(SignInError::PasswordRequired(pt)) => {
                Ok(SignInStep::PasswordRequired(PasswordToken::from_mt(pt)))
            }
            Err(e) => Err(Self::map_sign_in_error(e)),
        }
    }

    async fn check_password(
        &self,
        token: PasswordToken,
        password: &str,
    ) -> Result<PasswordStep, LoginError> {
        let mt = token.into_mt().ok_or_else(|| {
            LoginError::InvalidState("2FA token is unusable — restart the login".into())
        })?;
        match self.client.check_password(mt, password).await {
            Ok(user) => Ok(PasswordStep::Authorized(TgUserDto::from_user(&user))),
            // Telegram вернул НОВЫЙ токен — отдаём его наверх, чтобы
            // положить обратно в состояние (см. шапку модуля).
            Err(SignInError::InvalidPassword(new_token)) => Ok(PasswordStep::Invalid(
                PasswordToken::from_mt(new_token),
            )),
            Err(e) => Err(Self::map_sign_in_error(e)),
        }
    }

    async fn sign_out(&self) -> Result<(), LoginError> {
        self.client
            .sign_out()
            .await
            .map(|_| ())
            .map_err(|e| LoginError::from_invocation(&e))
    }

    async fn get_me(&self) -> Result<TgUserDto, LoginError> {
        self.client
            .get_me()
            .await
            .map(|u| TgUserDto::from_user(&u))
            .map_err(|e| LoginError::from_invocation(&e))
    }
}

// ---------------------------------------------------------------------------
// Переходы автомата
// ---------------------------------------------------------------------------

/// `Disconnected` ⟶ `CodeSent`: просит Telegram выслать код.
///
/// Разрешено также из `CodeSent` / `PasswordRequired` — это «отправить
/// код заново», нормальный сценарий, когда пользователь сменил телефон
/// или SMS не дошла. Из `Authorized` — нет: сначала logout.
///
/// # Errors
///
/// [`LoginError::InvalidState`] из `NotConfigured`/`Authorized`; всё
/// остальное — классификация ошибки Telegram (в т.ч. `FLOOD_WAIT`).
pub async fn request_code(
    state: &Arc<TelegramState>,
    backend: &dyn LoginBackend,
    phone: &str,
) -> Result<LoginStateDto, LoginError> {
    let phone = phone.trim();
    if phone.is_empty() {
        return Err(LoginError::BadInput("phone number is required".into()));
    }
    {
        let guard = state.login.lock().await;
        match &*guard {
            LoginFlow::NotConfigured => {
                return Err(LoginError::InvalidState(
                    "api_id/api_hash are not configured — save application credentials first"
                        .into(),
                ))
            }
            LoginFlow::Authorized { .. } => {
                return Err(LoginError::InvalidState(
                    "already signed in — log out before starting a new login".into(),
                ))
            }
            _ => {}
        }
    }

    let token = backend.request_login_code(phone).await?;
    state
        .set_login(LoginFlow::CodeSent {
            token: Some(token),
            phone: phone.to_string(),
        })
        .await;
    Ok(state.login_state_dto().await)
}

/// `CodeSent` ⟶ `Authorized` | `PasswordRequired`.
///
/// Токен НЕ расходуется: при неверном коде состояние остаётся `CodeSent`,
/// и пользователь вводит код заново без перезапуска wizard'а.
///
/// # Errors
///
/// [`LoginError::InvalidState`], если код прислали не из `CodeSent`;
/// [`LoginError::BadInput`] на неверном коде; `FLOOD_WAIT` — как есть.
pub async fn submit_code(
    state: &Arc<TelegramState>,
    backend: &dyn LoginBackend,
    code: &str,
) -> Result<LoginStateDto, LoginError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(LoginError::BadInput("confirmation code is required".into()));
    }

    // Токен изымается из состояния на время RPC: держать `Mutex` во
    // время сетевого вызова нельзя — на нём же стоят `GET /status` и
    // первый кадр WS. `sign_in` токен НЕ расходует, поэтому при любом
    // исходе, кроме перехода дальше, он кладётся обратно.
    let (token, phone) = {
        let mut guard = state.login.lock().await;
        match &mut *guard {
            LoginFlow::CodeSent { token, phone } => match token.take() {
                Some(t) => (t, phone.clone()),
                None => {
                    return Err(LoginError::InvalidState(
                        "another sign-in attempt is already in progress".into(),
                    ))
                }
            },
            other => {
                return Err(LoginError::InvalidState(format!(
                    "cannot submit the login code from state '{}': request a code first",
                    other.kind()
                )))
            }
        }
    };

    match backend.sign_in(&token, code).await {
        Ok(SignInStep::Authorized(user)) => {
            state.set_login(LoginFlow::Authorized { user }).await;
            Ok(state.login_state_dto().await)
        }
        Ok(SignInStep::PasswordRequired(pw_token)) => {
            let hint = pw_token.hint().map(str::to_string);
            state
                .set_login(LoginFlow::PasswordRequired {
                    token: Some(pw_token),
                    hint,
                })
                .await;
            Ok(state.login_state_dto().await)
        }
        Err(e) => {
            // Неверный код — не повод терять токен: возвращаем состояние
            // как было, и пользователь вводит код заново.
            state
                .set_login(LoginFlow::CodeSent {
                    token: Some(token),
                    phone,
                })
                .await;
            Err(e)
        }
    }
}

/// `PasswordRequired` ⟶ `Authorized`.
///
/// Токен расходуется всегда. При неверном пароле Telegram отдаёт НОВЫЙ
/// токен — он кладётся обратно, и пользователь повторяет ввод. При любой
/// другой ошибке токен потерян: состояние становится
/// `PasswordRequired{token: None}`, что для UI означает «начать заново».
///
/// # Errors
///
/// [`LoginError::InvalidState`] вне `PasswordRequired` (или когда токен
/// уже утерян), [`LoginError::BadInput`] на неверном пароле, прочее — по
/// классификации ошибки Telegram.
pub async fn submit_password(
    state: &Arc<TelegramState>,
    backend: &dyn LoginBackend,
    password: &str,
) -> Result<LoginStateDto, LoginError> {
    if password.is_empty() {
        return Err(LoginError::BadInput("2FA password is required".into()));
    }

    // Токен ЗАБИРАЕТСЯ из состояния: check_password его расходует.
    let (token, hint) = {
        let mut guard = state.login.lock().await;
        match &mut *guard {
            LoginFlow::PasswordRequired { token, hint } => match token.take() {
                Some(t) => (t, hint.clone()),
                None => {
                    return Err(LoginError::InvalidState(
                        "the 2FA session expired — restart the login".into(),
                    ))
                }
            },
            other => {
                return Err(LoginError::InvalidState(format!(
                    "cannot submit the 2FA password from state '{}'",
                    other.kind()
                )))
            }
        }
    };

    match backend.check_password(token, password).await {
        Ok(PasswordStep::Authorized(user)) => {
            state.set_login(LoginFlow::Authorized { user }).await;
            Ok(state.login_state_dto().await)
        }
        Ok(PasswordStep::Invalid(new_token)) => {
            // Новый токен ОБЯЗАТЕЛЬНО возвращается в состояние, иначе
            // одна опечатка стоила бы всего логина.
            let hint = new_token.hint().map(str::to_string).or(hint);
            state
                .set_login(LoginFlow::PasswordRequired {
                    token: Some(new_token),
                    hint,
                })
                .await;
            Err(LoginError::BadInput(
                "the 2FA password is invalid — try again".into(),
            ))
        }
        Err(e) => {
            // Токен израсходован и не вернулся: логин придётся начинать
            // заново, о чём сообщает token: None.
            state
                .set_login(LoginFlow::PasswordRequired { token: None, hint })
                .await;
            Err(e)
        }
    }
}

/// `Authorized` ⟶ `Disconnected` / `NotConfigured`.
///
/// `sign_out` делается best-effort: даже если Telegram не ответил,
/// локальное состояние всё равно переводится в «не авторизован» —
/// пользователь нажал «выйти», и UI обязан это отразить.
pub async fn sign_out(
    state: &Arc<TelegramState>,
    backend: Option<&dyn LoginBackend>,
) -> LoginStateDto {
    if let Some(backend) = backend {
        if let Err(e) = backend.sign_out().await {
            tracing::warn!(
                target: "forge_telegram",
                error = %e,
                "forge-telegram: sign_out request failed; clearing local state anyway"
            );
        }
    }
    let next = if state.has_credentials().await {
        LoginFlow::Disconnected { error: None }
    } else {
        LoginFlow::NotConfigured
    };
    state.set_login(next).await;
    state.login_state_dto().await
}

/// Сессия перестала быть валидной (`AUTH_KEY_UNREGISTERED`, разлогин с
/// другого устройства, смена пароля).
///
/// Вызывается updates-воркером (Фаза 3, риск №4 плана) и восстановлением
/// сессии при старте. Идемпотентна.
pub async fn on_auth_invalidated(state: &Arc<TelegramState>, reason: impl Into<String>) {
    let reason = reason.into();
    tracing::warn!(
        target: "forge_telegram",
        reason = %reason,
        "forge-telegram: Telegram authorization invalidated"
    );
    let next = if state.has_credentials().await {
        LoginFlow::Disconnected {
            error: Some(reason),
        }
    } else {
        LoginFlow::NotConfigured
    };
    state.set_login(next).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn user(id: &str) -> TgUserDto {
        TgUserDto {
            id: id.into(),
            username: Some("forge".into()),
            first_name: Some("Forge".into()),
            phone: Some("+79990000000".into()),
        }
    }

    fn all_variants() -> Vec<LoginFlow> {
        vec![
            LoginFlow::NotConfigured,
            LoginFlow::Disconnected {
                error: Some("connection reset".into()),
            },
            LoginFlow::CodeSent {
                token: Some(LoginToken::stub("super-secret-login-token")),
                phone: "+79990000000".into(),
            },
            LoginFlow::PasswordRequired {
                token: Some(PasswordToken::stub(
                    "super-secret-password-token",
                    Some("dog name"),
                )),
                hint: Some("dog name".into()),
            },
            LoginFlow::Authorized { user: user("1234567890123") },
        ]
    }

    /// Сценарий, который отыгрывает фейковый бэкенд по шагам.
    enum Step {
        Code(Result<(), LoginError>),
        SignIn(Result<SignInStep, LoginError>),
        Password(Result<PasswordStep, LoginError>),
        SignOut(Result<(), LoginError>),
    }

    struct FakeBackend {
        steps: StdMutex<Vec<Step>>,
        calls: StdMutex<Vec<&'static str>>,
    }

    impl FakeBackend {
        fn new(steps: Vec<Step>) -> Self {
            Self {
                steps: StdMutex::new(steps.into_iter().rev().collect()),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn pop(&self, what: &'static str) -> Step {
            self.calls.lock().unwrap().push(what);
            self.steps
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| panic!("сценарий закончился на вызове {what}"))
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LoginBackend for FakeBackend {
        async fn request_login_code(&self, _phone: &str) -> Result<LoginToken, LoginError> {
            match self.pop("code") {
                Step::Code(Ok(())) => Ok(LoginToken::stub("login-token")),
                Step::Code(Err(e)) => Err(e),
                _ => panic!("ожидался шаг request_login_code"),
            }
        }

        async fn sign_in(&self, _t: &LoginToken, _code: &str) -> Result<SignInStep, LoginError> {
            match self.pop("sign_in") {
                Step::SignIn(r) => r,
                _ => panic!("ожидался шаг sign_in"),
            }
        }

        async fn check_password(
            &self,
            _token: PasswordToken,
            _password: &str,
        ) -> Result<PasswordStep, LoginError> {
            match self.pop("check_password") {
                Step::Password(r) => r,
                _ => panic!("ожидался шаг check_password"),
            }
        }

        async fn sign_out(&self) -> Result<(), LoginError> {
            match self.pop("sign_out") {
                Step::SignOut(r) => r,
                _ => panic!("ожидался шаг sign_out"),
            }
        }

        async fn get_me(&self) -> Result<TgUserDto, LoginError> {
            self.calls.lock().unwrap().push("get_me");
            Ok(user("1"))
        }
    }

    fn configured_state() -> Arc<TelegramState> {
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            ..TelegramConfig::default()
        };
        TelegramState::new(cfg)
    }

    // --- чистая часть: kind / to_dto / from_config -------------------------

    #[test]
    fn kind_covers_every_variant() {
        let kinds: Vec<&str> = all_variants().iter().map(|f| f.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                "not_configured",
                "disconnected",
                "code_sent",
                "password_required",
                "authorized",
            ]
        );
    }

    #[test]
    fn dto_never_leaks_tokens() {
        for flow in all_variants() {
            let json = serde_json::to_string(&flow.to_dto()).unwrap();
            assert!(
                !json.contains("token"),
                "поле token просочилось наружу: {json}"
            );
            assert!(
                !json.contains("super-secret"),
                "значение токена просочилось наружу: {json}"
            );
        }
    }

    #[test]
    fn debug_of_tokens_is_redacted() {
        let t = LoginToken::stub("super-secret-login-token");
        assert!(!format!("{t:?}").contains("super-secret"), "{t:?}");
        let p = PasswordToken::stub("super-secret-password-token", Some("dog"));
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("super-secret"), "{dbg}");
        assert!(dbg.contains("dog"), "подсказка не секрет: {dbg}");
    }

    #[test]
    fn dto_carries_only_relevant_fields() {
        let dto = LoginFlow::Disconnected {
            error: Some("boom".into()),
        }
        .to_dto();
        assert_eq!(dto.state, "disconnected");
        assert_eq!(dto.error.as_deref(), Some("boom"));
        assert!(dto.phone.is_none() && dto.hint.is_none() && dto.user.is_none());

        let dto = LoginFlow::CodeSent {
            token: Some(LoginToken::stub("t")),
            phone: "+7000".into(),
        }
        .to_dto();
        assert_eq!(dto.phone.as_deref(), Some("+7000"));
        assert!(dto.error.is_none() && dto.user.is_none());

        let dto = LoginFlow::PasswordRequired {
            token: None,
            hint: Some("hint".into()),
        }
        .to_dto();
        assert_eq!(dto.hint.as_deref(), Some("hint"));

        let dto = LoginFlow::Authorized { user: user("42") }.to_dto();
        assert_eq!(dto.user.unwrap().id, "42");
    }

    #[test]
    fn from_config_derives_initial_state() {
        let cfg = TelegramConfig::default();
        assert_eq!(LoginFlow::from_config(&cfg).kind(), "not_configured");

        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            ..TelegramConfig::default()
        };
        assert_eq!(LoginFlow::from_config(&cfg).kind(), "disconnected");

        // Только один из кредов — этого недостаточно.
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: None,
            ..TelegramConfig::default()
        };
        assert_eq!(LoginFlow::from_config(&cfg).kind(), "not_configured");
    }

    #[test]
    fn is_authorized_and_is_configured() {
        assert!(!LoginFlow::NotConfigured.is_configured());
        assert!(LoginFlow::Disconnected { error: None }.is_configured());
        assert!(!LoginFlow::Disconnected { error: None }.is_authorized());
        assert!(LoginFlow::Authorized { user: user("1") }.is_authorized());
    }

    #[test]
    fn login_state_dto_round_trips() {
        let dto = LoginFlow::Authorized { user: user("777") }.to_dto();
        let json = serde_json::to_string(&dto).unwrap();
        let back: LoginStateDto = serde_json::from_str(&json).unwrap();
        assert_eq!(dto, back);
    }

    // --- матрица переходов -------------------------------------------------

    /// 1. Счастливый путь без 2FA: код → авторизован.
    #[tokio::test]
    async fn code_then_authorized() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::Authorized(user("100")))),
        ]);

        let dto = request_code(&st, &be, "+79990000000").await.unwrap();
        assert_eq!(dto.state, "code_sent");
        assert_eq!(dto.phone.as_deref(), Some("+79990000000"));

        let dto = submit_code(&st, &be, "12345").await.unwrap();
        assert_eq!(dto.state, "authorized");
        assert_eq!(dto.user.unwrap().id, "100");
        assert!(st.is_authorized().await);
    }

    /// 2. Путь с 2FA: код → пароль → авторизован; hint доезжает до DTO.
    #[tokio::test]
    async fn code_then_password_then_authorized() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::PasswordRequired(PasswordToken::stub(
                "pt-1",
                Some("dog name"),
            )))),
            Step::Password(Ok(PasswordStep::Authorized(user("200")))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        let dto = submit_code(&st, &be, "12345").await.unwrap();
        assert_eq!(dto.state, "password_required");
        assert_eq!(dto.hint.as_deref(), Some("dog name"));

        let dto = submit_password(&st, &be, "hunter2").await.unwrap();
        assert_eq!(dto.state, "authorized");
        assert_eq!(dto.user.unwrap().id, "200");
    }

    /// 3. Ключевой кейс задачи: неверный пароль ДВАЖДЫ подряд, затем
    ///    верный → Authorized. Каждая неудача обязана вернуть новый токен
    ///    в состояние, иначе третья попытка была бы невозможна.
    #[tokio::test]
    async fn two_wrong_passwords_then_success() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::PasswordRequired(PasswordToken::stub(
                "pt-1",
                Some("hint"),
            )))),
            Step::Password(Ok(PasswordStep::Invalid(PasswordToken::stub(
                "pt-2",
                Some("hint"),
            )))),
            Step::Password(Ok(PasswordStep::Invalid(PasswordToken::stub(
                "pt-3",
                Some("hint"),
            )))),
            Step::Password(Ok(PasswordStep::Authorized(user("300")))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        submit_code(&st, &be, "12345").await.unwrap();

        // После каждой неудачи в состоянии обязан оказаться ИМЕННО новый
        // токен, который Telegram прислал вместе с ошибкой (pt-2, затем
        // pt-3) — старый уже израсходован и второй раз не сработает.
        for expected in ["pt-2", "pt-3"] {
            let err = submit_password(&st, &be, "wrong").await.unwrap_err();
            assert!(matches!(err, LoginError::BadInput(_)), "{err:?}");
            let guard = st.login.lock().await;
            match &*guard {
                LoginFlow::PasswordRequired { token, hint } => {
                    let token = token.as_ref().expect("новый токен обязан лечь обратно");
                    assert_eq!(token.stub_tag(), Some(expected));
                    assert_eq!(hint.as_deref(), Some("hint"));
                }
                other => panic!("неожиданное состояние: {other:?}"),
            }
        }

        let dto = submit_password(&st, &be, "hunter2").await.unwrap();
        assert_eq!(dto.state, "authorized");
        assert_eq!(
            be.calls(),
            vec![
                "code",
                "sign_in",
                "check_password",
                "check_password",
                "check_password"
            ]
        );
    }

    /// 4. Неверный код: состояние остаётся CodeSent (токен не расходуется),
    ///    повторный ввод проходит без перезапуска wizard'а.
    #[tokio::test]
    async fn wrong_code_keeps_code_sent_state() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Err(LoginError::BadInput("bad code".into()))),
            Step::SignIn(Ok(SignInStep::Authorized(user("400")))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        let err = submit_code(&st, &be, "00000").await.unwrap_err();
        assert!(matches!(err, LoginError::BadInput(_)), "{err:?}");
        assert_eq!(st.login_state_dto().await.state, "code_sent");
        {
            // Тот же самый токен вернулся в состояние: sign_in его не
            // расходует, повтор возможен без нового запроса кода.
            let guard = st.login.lock().await;
            match &*guard {
                LoginFlow::CodeSent { token, phone } => {
                    assert_eq!(
                        token.as_ref().and_then(|t| t.stub_tag()),
                        Some("login-token")
                    );
                    assert_eq!(phone, "+79990000000");
                }
                other => panic!("неожиданное состояние: {other:?}"),
            }
        }

        let dto = submit_code(&st, &be, "12345").await.unwrap();
        assert_eq!(dto.state, "authorized");
    }

    /// 5. FLOOD_WAIT на запросе кода: состояние НЕ меняется, ошибка несёт
    ///    секунды ожидания.
    #[tokio::test]
    async fn flood_wait_on_request_code_keeps_state() {
        let st = configured_state();
        let be = FakeBackend::new(vec![Step::Code(Err(LoginError::flood(65)))]);

        let err = request_code(&st, &be, "+79990000000").await.unwrap_err();
        match err {
            LoginError::FloodWait { secs, .. } => assert_eq!(secs, 65),
            other => panic!("ожидался FloodWait: {other:?}"),
        }
        assert_eq!(st.login_state_dto().await.state, "disconnected");
    }

    /// 6. Ошибка check_password, отличная от «неверный пароль», теряет
    ///    токен — состояние обязано честно сказать об этом (`token: None`).
    #[tokio::test]
    async fn failed_password_step_marks_token_lost() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::PasswordRequired(PasswordToken::stub(
                "pt-1", None,
            )))),
            Step::Password(Err(LoginError::flood(30))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        submit_code(&st, &be, "12345").await.unwrap();
        let err = submit_password(&st, &be, "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::FloodWait { secs: 30, .. }), "{err:?}");

        {
            let guard = st.login.lock().await;
            match &*guard {
                LoginFlow::PasswordRequired { token, .. } => assert!(token.is_none()),
                other => panic!("неожиданное состояние: {other:?}"),
            }
        }
        // Повторная попытка честно отвечает invalid_state, а не паникует.
        let err = submit_password(&st, &be, "hunter2").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");
    }

    /// 7. Шаги вне своего состояния: код без CodeSent, пароль без
    ///    PasswordRequired, старт логина из NotConfigured и Authorized.
    #[tokio::test]
    async fn steps_out_of_order_are_rejected() {
        let st = configured_state();
        let be = FakeBackend::new(vec![]);

        let err = submit_code(&st, &be, "12345").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");
        let err = submit_password(&st, &be, "pw").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");

        let unconfigured = TelegramState::new(TelegramConfig::default());
        let err = request_code(&unconfigured, &be, "+7999").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");

        st.set_login(LoginFlow::Authorized { user: user("1") }).await;
        let err = request_code(&st, &be, "+7999").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");
        // Ни один из отказов не дошёл до сети.
        assert!(be.calls().is_empty());
    }

    /// 8. Пустой ввод отбивается до похода в сеть.
    #[tokio::test]
    async fn empty_input_is_rejected_locally() {
        let st = configured_state();
        let be = FakeBackend::new(vec![]);

        assert!(matches!(
            request_code(&st, &be, "   ").await.unwrap_err(),
            LoginError::BadInput(_)
        ));
        assert!(matches!(
            submit_code(&st, &be, "  ").await.unwrap_err(),
            LoginError::BadInput(_)
        ));
        assert!(matches!(
            submit_password(&st, &be, "").await.unwrap_err(),
            LoginError::BadInput(_)
        ));
        assert!(be.calls().is_empty());
    }

    /// 9. logout: даже при ошибке sign_out локальное состояние сбрасывается.
    #[tokio::test]
    async fn sign_out_clears_state_even_when_request_fails() {
        let st = configured_state();
        st.set_login(LoginFlow::Authorized { user: user("1") }).await;
        let be = FakeBackend::new(vec![Step::SignOut(Err(LoginError::NotConnected(
            "no network".into(),
        )))]);

        let dto = sign_out(&st, Some(&be)).await;
        assert_eq!(dto.state, "disconnected");
        assert!(!st.is_authorized().await);

        // Без кредов выход возвращает в not_configured.
        let unconfigured = TelegramState::new(TelegramConfig::default());
        unconfigured
            .set_login(LoginFlow::Authorized { user: user("1") })
            .await;
        let dto = sign_out(&unconfigured, None).await;
        assert_eq!(dto.state, "not_configured");
    }

    /// 10. Инвалидация сессии переводит в Disconnected с причиной.
    #[tokio::test]
    async fn auth_invalidated_moves_to_disconnected() {
        let st = configured_state();
        st.set_login(LoginFlow::Authorized { user: user("1") }).await;
        let mut rx = st.subscribe();

        on_auth_invalidated(&st, "AUTH_KEY_UNREGISTERED").await;

        let dto = st.login_state_dto().await;
        assert_eq!(dto.state, "disconnected");
        assert_eq!(dto.error.as_deref(), Some("AUTH_KEY_UNREGISTERED"));

        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            crate::ws::protocol::TgServerMsg::LoginState(d) => {
                assert_eq!(d.state, "disconnected")
            }
            other => panic!("ожидался login_state: {other:?}"),
        }
    }

    /// 11. Каждый успешный переход виден подписчикам WS в правильном порядке.
    #[tokio::test]
    async fn every_transition_broadcasts_login_state() {
        let st = configured_state();
        let mut rx = st.subscribe();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::PasswordRequired(PasswordToken::stub(
                "pt", None,
            )))),
            Step::Password(Ok(PasswordStep::Authorized(user("500")))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        submit_code(&st, &be, "12345").await.unwrap();
        submit_password(&st, &be, "hunter2").await.unwrap();

        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let crate::ws::protocol::TgServerMsg::LoginState(d) = ev {
                seen.push(d.state);
            }
        }
        assert_eq!(seen, vec!["code_sent", "password_required", "authorized"]);
    }

    /// 12. Классификация ошибок RPC в термины REST-контракта.
    #[test]
    fn invocation_errors_are_classified() {
        use grammers_tl_types as tl;
        let rpc = |code: i32, msg: &str| {
            InvocationError::Rpc(grammers_mtsender::RpcError::from(tl::types::RpcError {
                error_code: code,
                error_message: msg.to_string(),
            }))
        };

        assert!(matches!(
            LoginError::from_invocation(&rpc(420, "FLOOD_WAIT_31")),
            LoginError::FloodWait { secs: 31, .. }
        ));
        assert!(matches!(
            LoginError::from_invocation(&rpc(400, "PHONE_CODE_INVALID")),
            LoginError::BadInput(_)
        ));
        assert!(matches!(
            LoginError::from_invocation(&rpc(401, "AUTH_KEY_UNREGISTERED")),
            LoginError::InvalidState(_)
        ));
        assert!(matches!(
            LoginError::from_invocation(&InvocationError::Dropped),
            LoginError::NotConnected(_)
        ));
    }

    /// 13. Выход возможен ИЗ ЛЮБОГО состояния и всегда приводит к одному и
    ///     тому же итогу.
    ///
    /// Кнопка ⏻ показывается только авторизованному, но `POST /logout`
    /// доступен всегда (и им пользуется фронт при обнаружении рассинхрона).
    /// Поэтому проверяется вся матрица: из каждого варианта [`LoginFlow`]
    /// выход обязан оставить `disconnected` (креды есть — вход можно
    /// повторить) и НЕ оставить ни одного токена в памяти.
    #[tokio::test]
    async fn sign_out_from_every_state_lands_in_disconnected() {
        for start in all_variants() {
            let kind = start.kind();
            let st = configured_state();
            st.set_login(start).await;

            let be = FakeBackend::new(vec![Step::SignOut(Ok(()))]);
            let dto = sign_out(&st, Some(&be)).await;

            assert_eq!(dto.state, "disconnected", "выход из {kind}");
            assert!(!st.is_authorized().await, "после выхода из {kind}");
            let json = serde_json::to_string(&dto).unwrap();
            assert!(!json.contains("token"), "из {kind} утёк токен: {json}");

            let guard = st.login.lock().await;
            assert!(
                matches!(&*guard, LoginFlow::Disconnected { .. }),
                "из {kind} получили {:?}",
                &*guard
            );
        }
    }

    /// 14. То же самое, но без кредов: выход возвращает в `not_configured`.
    ///
    /// Разница принципиальна для фронта: `disconnected` показывает форму
    /// «введите телефон», `not_configured` — форму «введите api_id/api_hash».
    #[tokio::test]
    async fn sign_out_without_credentials_returns_to_not_configured() {
        for start in all_variants() {
            let kind = start.kind();
            let st = TelegramState::new(TelegramConfig::default());
            st.set_login(start).await;

            let dto = sign_out(&st, None).await;
            assert_eq!(dto.state, "not_configured", "выход из {kind} без кредов");
        }
    }

    /// 15. Инвалидация сессии приходит НЕ только из Authorized.
    ///
    /// `AUTH_KEY_UNREGISTERED` может прилететь и на середине wizard'а (сессию
    /// отозвали с телефона, пока пользователь искал код). Из любого состояния
    /// результат один — `disconnected` с причиной, и никаких залипших токенов.
    #[tokio::test]
    async fn auth_invalidated_from_every_state() {
        for start in all_variants() {
            let kind = start.kind();
            let st = configured_state();
            st.set_login(start).await;

            on_auth_invalidated(&st, "AUTH_KEY_UNREGISTERED").await;

            let dto = st.login_state_dto().await;
            assert_eq!(dto.state, "disconnected", "инвалидация из {kind}");
            assert_eq!(dto.error.as_deref(), Some("AUTH_KEY_UNREGISTERED"));
        }
    }

    /// 16. Каждое состояние переживает round-trip своего DTO.
    ///
    /// Тот же кадр летит и в HTTP-ответе, и в WS-событии `login_state`;
    /// расхождение сериализации между ними означало бы, что фронт видит
    /// разное состояние по разным каналам.
    #[test]
    fn every_state_dto_round_trips() {
        for flow in all_variants() {
            let dto = flow.to_dto();
            let json = serde_json::to_string(&dto).unwrap();
            let back: LoginStateDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back.state, dto.state, "{json}");
            assert_eq!(back.phone, dto.phone, "{json}");
            assert_eq!(back.hint, dto.hint, "{json}");
            assert_eq!(back.error, dto.error, "{json}");
        }
    }

    /// 17. Повторный request_code из CodeSent — «отправить код ещё раз».
    ///
    /// Реальный сценарий: SMS не дошла или пользователь опечатался в
    /// номере и вводит другой, не перезапуская wizard. Старый токен обязан
    /// быть заменён свежим (код от нового запроса не подходит к старому
    /// токену), а телефон в состоянии — обновиться.
    #[tokio::test]
    async fn resend_code_replaces_token_and_phone() {
        let st = configured_state();
        st.set_login(LoginFlow::CodeSent {
            token: Some(LoginToken::stub("stale-token")),
            phone: "+79990000001".into(),
        })
        .await;
        let be = FakeBackend::new(vec![Step::Code(Ok(()))]);

        let dto = request_code(&st, &be, "+79990000002").await.unwrap();
        assert_eq!(dto.state, "code_sent");
        assert_eq!(dto.phone.as_deref(), Some("+79990000002"));

        let guard = st.login.lock().await;
        match &*guard {
            LoginFlow::CodeSent { token, phone } => {
                assert_eq!(
                    token.as_ref().and_then(|t| t.stub_tag()),
                    Some("login-token"),
                    "старый токен обязан быть заменён свежим"
                );
                assert_eq!(phone, "+79990000002");
            }
            other => panic!("неожиданное состояние: {other:?}"),
        }
    }

    /// 18. request_code из PasswordRequired — рестарт wizard'а с шага кода.
    ///
    /// Пользователь забыл 2FA-пароль и решил начать заново: переход
    /// разрешён, а подсказка прошлой 2FA-сессии не должна пережить рестарт
    /// (иначе UI показал бы hint у формы ввода кода).
    #[tokio::test]
    async fn resend_code_from_password_required_restarts_wizard() {
        let st = configured_state();
        st.set_login(LoginFlow::PasswordRequired {
            token: Some(PasswordToken::stub("pt-stale", Some("dog name"))),
            hint: Some("dog name".into()),
        })
        .await;
        let be = FakeBackend::new(vec![Step::Code(Ok(()))]);

        let dto = request_code(&st, &be, "+79990000000").await.unwrap();
        assert_eq!(dto.state, "code_sent");
        assert!(
            dto.hint.is_none(),
            "hint прошлой 2FA-сессии пережил рестарт: {dto:?}"
        );
    }

    /// 19. Гонка двойного submit_code (двойной клик по «Войти»).
    ///
    /// Пока первый sign_in висит «в сети», токен изъят из состояния.
    /// Второй вызов обязан получить invalid_state, а не встать в очередь
    /// за токеном; и сам Mutex в это время свободен — `GET /status`
    /// отвечает мгновенно, не дожидаясь сети.
    #[tokio::test]
    async fn concurrent_submit_code_rejects_second_caller() {
        struct BlockingBackend {
            entered: tokio::sync::mpsc::UnboundedSender<()>,
            release: Arc<tokio::sync::Notify>,
        }

        #[async_trait]
        impl LoginBackend for BlockingBackend {
            async fn request_login_code(&self, _p: &str) -> Result<LoginToken, LoginError> {
                Ok(LoginToken::stub("login-token"))
            }

            async fn sign_in(
                &self,
                _t: &LoginToken,
                _c: &str,
            ) -> Result<SignInStep, LoginError> {
                let _ = self.entered.send(());
                self.release.notified().await;
                Ok(SignInStep::Authorized(user("900")))
            }

            async fn check_password(
                &self,
                _t: PasswordToken,
                _p: &str,
            ) -> Result<PasswordStep, LoginError> {
                unreachable!("в этом сценарии пароль не запрашивается")
            }

            async fn sign_out(&self) -> Result<(), LoginError> {
                Ok(())
            }

            async fn get_me(&self) -> Result<TgUserDto, LoginError> {
                Ok(user("900"))
            }
        }

        let st = configured_state();
        let (tx, mut entered) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let be = Arc::new(BlockingBackend {
            entered: tx,
            release: Arc::clone(&release),
        });

        request_code(&st, be.as_ref(), "+79990000000").await.unwrap();

        let st2 = Arc::clone(&st);
        let be2 = Arc::clone(&be);
        let first =
            tokio::spawn(async move { submit_code(&st2, be2.as_ref(), "12345").await });

        // Дожидаемся, пока первый вызов реально повис в «сети».
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.recv())
            .await
            .expect("sign_in так и не начался")
            .unwrap();

        // Mutex состояния свободен: статус отвечает, не дожидаясь RPC.
        let dto = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            st.login_state_dto(),
        )
        .await
        .expect("login_state_dto завис на Mutex во время сетевого вызова");
        assert_eq!(dto.state, "code_sent");

        // Второй клик — честный invalid_state, а не гонка за токен.
        let err = submit_code(&st, be.as_ref(), "12345").await.unwrap_err();
        assert!(matches!(err, LoginError::InvalidState(_)), "{err:?}");

        release.notify_one();
        let dto = first.await.unwrap().unwrap();
        assert_eq!(dto.state, "authorized");
    }

    /// 20. Неверный пароль, а новый токен пришёл БЕЗ подсказки: старая
    ///     подсказка остаётся в состоянии.
    ///
    /// После опечатки UI обязан продолжать показывать hint — потеря
    /// подсказки при каждой неудачной попытке выглядела бы как баг формы.
    #[tokio::test]
    async fn wrong_password_without_new_hint_keeps_old_hint() {
        let st = configured_state();
        let be = FakeBackend::new(vec![
            Step::Code(Ok(())),
            Step::SignIn(Ok(SignInStep::PasswordRequired(PasswordToken::stub(
                "pt-1",
                Some("dog name"),
            )))),
            Step::Password(Ok(PasswordStep::Invalid(PasswordToken::stub("pt-2", None)))),
        ]);

        request_code(&st, &be, "+79990000000").await.unwrap();
        submit_code(&st, &be, "12345").await.unwrap();
        let err = submit_password(&st, &be, "typo").await.unwrap_err();
        assert!(matches!(err, LoginError::BadInput(_)), "{err:?}");

        let dto = st.login_state_dto().await;
        assert_eq!(dto.state, "password_required");
        assert_eq!(
            dto.hint.as_deref(),
            Some("dog name"),
            "hint потерян после опечатки"
        );
        let guard = st.login.lock().await;
        match &*guard {
            LoginFlow::PasswordRequired { token, .. } => {
                assert_eq!(token.as_ref().and_then(|t| t.stub_tag()), Some("pt-2"));
            }
            other => panic!("неожиданное состояние: {other:?}"),
        }
    }

    /// 21. Телефон и код чистятся trim'ом, пароль уходит В ТОЧНОСТИ как введён.
    ///
    /// Реальные вводы: телефон из буфера обмена с `\n`, код с пробелами от
    /// мобильной клавиатуры — их чистить нужно. Но 2FA-пароль может
    /// ЛЕГАЛЬНО начинаться или заканчиваться пробелом: trim здесь ломал бы
    /// вход без какой-либо диагностики.
    #[tokio::test]
    async fn inputs_are_trimmed_except_password() {
        #[derive(Default)]
        struct CapturingBackend {
            seen: StdMutex<Vec<(&'static str, String)>>,
        }

        #[async_trait]
        impl LoginBackend for CapturingBackend {
            async fn request_login_code(&self, phone: &str) -> Result<LoginToken, LoginError> {
                self.seen.lock().unwrap().push(("phone", phone.to_string()));
                Ok(LoginToken::stub("login-token"))
            }

            async fn sign_in(
                &self,
                _t: &LoginToken,
                code: &str,
            ) -> Result<SignInStep, LoginError> {
                self.seen.lock().unwrap().push(("code", code.to_string()));
                Ok(SignInStep::PasswordRequired(PasswordToken::stub("pt", None)))
            }

            async fn check_password(
                &self,
                _t: PasswordToken,
                password: &str,
            ) -> Result<PasswordStep, LoginError> {
                self.seen
                    .lock()
                    .unwrap()
                    .push(("password", password.to_string()));
                Ok(PasswordStep::Authorized(user("700")))
            }

            async fn sign_out(&self) -> Result<(), LoginError> {
                Ok(())
            }

            async fn get_me(&self) -> Result<TgUserDto, LoginError> {
                Ok(user("700"))
            }
        }

        let st = configured_state();
        let be = CapturingBackend::default();

        request_code(&st, &be, "\n +79990000000 \t").await.unwrap();
        submit_code(&st, &be, " 12345 ").await.unwrap();
        submit_password(&st, &be, "  p@ss with spaces  ").await.unwrap();

        let seen = be.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                ("phone", "+79990000000".to_string()),
                ("code", "12345".to_string()),
                ("password", "  p@ss with spaces  ".to_string()),
            ]
        );
    }

    /// 22. Инвалидация сессии, когда кредов уже нет: `not_configured`,
    ///     а не `disconnected`.
    ///
    /// Разница видима фронту: `disconnected` рисует форму телефона,
    /// `not_configured` — форму api_id/api_hash. Ошибку логина форме
    /// кредов показывать негде — DTO обязан прийти чистым.
    #[tokio::test]
    async fn auth_invalidated_without_credentials_lands_in_not_configured() {
        let st = TelegramState::new(TelegramConfig::default());
        st.set_login(LoginFlow::Authorized { user: user("1") }).await;

        on_auth_invalidated(&st, "SESSION_REVOKED").await;

        let dto = st.login_state_dto().await;
        assert_eq!(dto.state, "not_configured");
        assert!(dto.error.is_none(), "{dto:?}");
    }

    /// 23. FLOOD_WAIT распознаётся раньше классификации по HTTP-коду, а
    ///     патологические значения деградируют предсказуемо.
    #[test]
    fn flood_wait_classification_edge_values() {
        use grammers_tl_types as tl;
        let rpc = |code: i32, msg: &str| {
            InvocationError::Rpc(grammers_mtsender::RpcError::from(tl::types::RpcError {
                error_code: code,
                error_message: msg.to_string(),
            }))
        };

        // Telegram изредка присылает FLOOD_WAIT с кодом 400 — порядок
        // веток важен: сначала flood, потом классификация по коду.
        assert!(matches!(
            LoginError::from_invocation(&rpc(400, "FLOOD_WAIT_5")),
            LoginError::FloodWait { secs: 5, .. }
        ));

        // FLOOD_WAIT_0 — реальный ответ сервера: ждать «0 секунд», но это
        // всё ещё 429, и текст обязан называть срок.
        match LoginError::from_invocation(&rpc(420, "FLOOD_WAIT_0")) {
            LoginError::FloodWait { secs, message } => {
                assert_eq!(secs, 0);
                assert!(message.contains("0s"), "{message}");
            }
            other => panic!("ожидался FloodWait: {other:?}"),
        }

        // Значение не влезает в u32 (сервер прислал мусор): это уже не
        // «подожди N секунд». Главное — не паника и не ложный 400.
        assert!(matches!(
            LoginError::from_invocation(&rpc(420, "FLOOD_WAIT_99999999999")),
            LoginError::NotConnected(_)
        ));
    }
}
