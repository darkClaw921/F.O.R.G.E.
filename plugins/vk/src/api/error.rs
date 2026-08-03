//! Ошибки транспорта VK, разбор конверта ответа и таблица кодов.
//!
//! Модуль отвечает на три вопроса и больше ни на что:
//!
//! 1. **Что вообще пришло от VK** — [`parse_envelope`], единственное место,
//!    где JSON превращается в `Result`.
//! 2. **Как показать это пользователю** — [`classify`] и [`to_api_error`],
//!    отображение кода VK в контракт [`crate::routes::error::ApiError`].
//! 3. **Повторять ли запрос** — [`retry_for`], политика ретраев.
//!
//! Все три — ЧИСТЫЕ функции без сети и без состояния, поэтому вся таблица
//! проверяется юнит-тестами, а не «когда-нибудь на живом аккаунте».
//!
//! # Инвариант безопасности: `error.request_params`
//!
//! VK возвращает в теле ошибки ВАШИ ЖЕ параметры запроса, включая
//! `access_token`:
//!
//! ```json
//! {"error":{"error_code":5,"error_msg":"User authorization failed",
//!           "request_params":[{"key":"access_token","value":"vk1.a.SECRET"},
//!                             {"key":"v","value":"5.199"}]}}
//! ```
//!
//! Это САМАЯ ВЕРОЯТНАЯ утечка токена во всём плагине: тело ошибки естественным
//! образом уезжает в `tracing::warn!`, в `ApiError::message` и оттуда в браузер
//! и в HAR-файл поддержки. Поэтому [`parse_envelope`] выбрасывает
//! `request_params` **целиком** ПЕРВЫМ ЖЕ действием — до того, как из тела
//! возьмётся хоть одно поле.
//!
//! Выбрасывается всё поле, а не «маскируется значение `access_token`»:
//! состав ключей `request_params` VK нигде не документирован и может
//! пополниться в любой момент, а маска по списку имён защищает только от
//! известного.
//!
//! # Чем VK принципиально отличается от Telegram
//!
//! **Ошибка VK приходит с HTTP 200.** Статус ответа не говорит ни о чём:
//! конверт `{"response":…}` / `{"error":{…}}` разбирается сам. Код `!= 200`
//! означает не «VK отказал», а «до VK не доехали» (прокси, captive portal,
//! балансировщик) — и трактуется как транспортный отказ.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::routes::error::ApiError;

// ---------------------------------------------------------------------------
// Коды VK, на которые завязана ЛОГИКА (а не только текст ошибки).
// ---------------------------------------------------------------------------

/// Авторизация не удалась: токен протух, отозван или скопирован с ошибкой.
/// Ведёт в [`crate::login::LoginFlow::InvalidToken`].
pub const CODE_AUTH_FAILED: i64 = 5;

/// Слишком много запросов в секунду. Это промах НАШЕГО троттлера, см.
/// [`retry_for`].
pub const CODE_TOO_MANY_PER_SECOND: i64 = 6;

/// Flood control — VK притормозил однотипные запросы. Автоповтор ЗАПРЕЩЁН.
pub const CODE_FLOOD_CONTROL: i64 = 9;

/// Внутренняя ошибка сервера VK. Класс «сетевая рябь», один повтор.
pub const CODE_INTERNAL_SERVER: i64 = 10;

/// Требуется ввод символов с картинки. Тело несёт `captcha_sid` + `captcha_img`.
pub const CODE_CAPTCHA: i64 = 14;

/// Access denied: у токена нет права `messages`. Ведёт в
/// [`crate::login::LoginFlow::NoMessagesScope`].
pub const CODE_ACCESS_DENIED: i64 = 15;

/// Требуется валидация пользователя. Тело несёт `redirect_uri`.
pub const CODE_VALIDATION_REQUIRED: i64 = 17;

/// Достигнут количественный лимит метода (часовой/суточный). Автоповтор
/// ЗАПРЕЩЁН.
pub const CODE_METHOD_QUOTA: i64 = 29;

/// Сколько ждать после кода 6 — секунда, ровно один «тик» лимита.
pub const RETRY_AFTER_TOO_MANY_PER_SECOND: u64 = 1;

/// Сколько ждать после кода 9. VK точное время не сообщает; минута — принятая
/// в клиентах оценка, за которую flood control обычно отпускает.
pub const RETRY_AFTER_FLOOD_CONTROL: u64 = 60;

/// Сколько ждать после кода 29. Лимиты метода у VK часовые, поэтому UI обязан
/// показать честный час, а не «попробуйте ещё раз».
pub const RETRY_AFTER_METHOD_QUOTA: u64 = 3600;

// ---------------------------------------------------------------------------
// Тип ошибки транспорта.
// ---------------------------------------------------------------------------

/// Ошибка обращения к VK.
///
/// Живёт ВНУТРИ плагина: наружу (в HTTP) она превращается
/// [`to_api_error`] в [`ApiError`]. Разделение нужно, потому что решения
/// «повторить» и «показать» принимаются на разных уровнях: транспорт знает
/// про ретраи, REST-слой — про контракт с фронтом.
///
/// `Display` и `Debug` реализованы так, чтобы их можно было безопасно
/// логировать: секретов в полях нет по построению (см. [`parse_envelope`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VkError {
    /// До VK не доехали: DNS, connect, таймаут, обрыв соединения, ошибка TLS.
    Transport {
        /// Текст ошибки `reqwest` (URL из него вычищен — см.
        /// [`crate::api::VkClient`]).
        reason: String,
    },
    /// HTTP-статус ответа не 200.
    ///
    /// Сам VK так почти не отвечает — это прокси, captive portal или
    /// балансировщик, поэтому класс тот же, что у [`VkError::Transport`].
    Http {
        /// Полученный статус.
        status: u16,
    },
    /// Тело ответа не разобралось: не JSON, либо JSON без `response` и без
    /// `error`.
    Decode {
        /// Что именно не сошлось (без самого тела: в нём могут быть данные
        /// пользователя).
        reason: String,
    },
    /// VK ответил конвертом ошибки (напоминаем: с HTTP 200).
    Api {
        /// `error_code`.
        code: i64,
        /// `error_msg`. `request_params` В ЭТУ СТРУКТУРУ НЕ ПОПАДАЕТ НИКОГДА.
        msg: String,
        /// `captcha_sid` (код 14).
        captcha_sid: Option<String>,
        /// `captcha_img` (код 14).
        captcha_img: Option<String>,
        /// `redirect_uri` (код 17).
        redirect_uri: Option<String>,
    },
}

impl fmt::Display for VkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { reason } => write!(f, "vk transport failure: {reason}"),
            Self::Http { status } => write!(f, "vk replied with HTTP {status}"),
            Self::Decode { reason } => write!(f, "vk response is not parseable: {reason}"),
            Self::Api { code, msg, .. } => write!(f, "vk error {code}: {msg}"),
        }
    }
}

impl std::error::Error for VkError {}

impl VkError {
    /// Код VK, если ошибка пришла конвертом.
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Api { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Транспортный ли это отказ.
    ///
    /// По нему [`crate::api::VkClient`] решает, трогать ли
    /// [`crate::state::ConnHealth`]: «сеть лежит» — это про
    /// [`VkError::Transport`] и [`VkError::Http`], а не про отказ VK по
    /// существу (тот приходит по живой сети).
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Http { .. })
    }

    /// Пара `(sid, img)` для кода 14, если VK прислал оба поля.
    pub fn captcha(&self) -> Option<(&str, &str)> {
        match self {
            Self::Api {
                captcha_sid: Some(sid),
                captcha_img: Some(img),
                ..
            } => Some((sid.as_str(), img.as_str())),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Разбор конверта.
// ---------------------------------------------------------------------------

/// ЕДИНСТВЕННОЕ место, где JSON от VK превращается в `Result`.
///
/// Правила В ПОРЯДКЕ ПРОВЕРКИ:
///
/// 1. `status != 200` ⇒ [`VkError::Http`] (трактуем как транспорт);
/// 2. тело не JSON ⇒ [`VkError::Decode`];
/// 3. есть ключ `"error"` ⇒ [`VkError::Api`] — **с предварительным удалением
///    `request_params`** (см. шапку модуля);
/// 4. есть ключ `"response"` ⇒ `Ok(value)`;
/// 5. иначе ⇒ [`VkError::Decode`].
///
/// Порядок 3 перед 4 важен: у VK бывают ответы, где есть ОБА ключа
/// (частичный успех `execute`), и приоритет ошибки честнее — иначе отказ
/// проехал бы как успех с урезанными данными.
///
/// Функция чистая: ни логов, ни состояния, ни сети. Всё, что нужно вызывающему,
/// возвращается значением.
pub fn parse_envelope(status: u16, body: &[u8]) -> Result<Value, VkError> {
    if status != 200 {
        return Err(VkError::Http { status });
    }
    let mut root: Value = serde_json::from_slice(body).map_err(|e| VkError::Decode {
        // В `reason` едет только описание ошибки serde (позиция и вид), но
        // НЕ само тело: в нём могут быть и данные пользователя, и — в ветке
        // ошибки — параметры запроса.
        reason: e.to_string(),
    })?;

    if let Some(err) = root.get_mut("error") {
        return Err(api_error_from_value(err));
    }
    match root.get_mut("response") {
        Some(v) => Ok(v.take()),
        None => Err(VkError::Decode {
            reason: "neither `response` nor `error` is present".to_string(),
        }),
    }
}

/// Разбор ответа upload-сервера VK.
///
/// Отдельная функция, а не [`parse_envelope`], потому что upload-сервер — это
/// НЕ API: он отвечает голым объектом `{"photo":…,"server":…,"hash":…}` без
/// конверта `response`. Прогнать такой ответ через [`parse_envelope`] значило
/// бы получить `Decode` на совершенно исправной загрузке.
///
/// Правила: `status != 200` ⇒ [`VkError::Http`]; не JSON ⇒ [`VkError::Decode`];
/// есть `"error"` ⇒ [`VkError::Api`] (с тем же удалением `request_params` —
/// upload-сервер тоже видел наши параметры); иначе — `Ok(value)` как есть.
pub fn parse_upload_response(status: u16, body: &[u8]) -> Result<Value, VkError> {
    if status != 200 {
        return Err(VkError::Http { status });
    }
    let mut root: Value = serde_json::from_slice(body).map_err(|e| VkError::Decode {
        reason: e.to_string(),
    })?;
    if let Some(err) = root.get_mut("error") {
        return Err(api_error_from_value(err));
    }
    Ok(root)
}

/// Собирает [`VkError::Api`] из значения ключа `error`.
///
/// ПЕРВОЕ ДЕЙСТВИЕ — удаление `request_params`. Всё остальное читается уже из
/// очищенного объекта, поэтому забыть про очистку в новой ветке физически
/// негде.
fn api_error_from_value(err: &mut Value) -> VkError {
    if let Some(obj) = err.as_object_mut() {
        // ↓↓↓ ИНВАРИАНТ БЕЗОПАСНОСТИ ФАЗЫ ↓↓↓
        obj.remove("request_params");
    }

    let Some(obj) = err.as_object() else {
        // `{"error":"invalid_request"}` — форма oauth-эндпоинта VK, а не
        // метода API. Кода у неё нет, поэтому 0: [`classify`] отдаст на него
        // честный `vk_error`.
        return VkError::Api {
            code: 0,
            msg: err.as_str().unwrap_or("unknown error").to_string(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
    };

    let code = obj.get("error_code").and_then(Value::as_i64).unwrap_or(0);
    let msg = obj
        .get("error_msg")
        .or_else(|| obj.get("error_description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    VkError::Api {
        code,
        msg,
        captcha_sid: scalar_str(obj, "captcha_sid"),
        captcha_img: scalar_str(obj, "captcha_img"),
        redirect_uri: scalar_str(obj, "redirect_uri"),
    }
}

/// Достаёт строковое поле, переживая присланное числом.
///
/// `captcha_sid` VK отдаёт то строкой, то числом в зависимости от метода —
/// строгий разбор ронял бы captcha ровно там, где она и так болезненна.
fn scalar_str(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match obj.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Таблица кодов.
// ---------------------------------------------------------------------------

/// Код VK → ошибка REST-контракта.
///
/// ЧИСТАЯ функция от одного числа: сообщение здесь каноническое (текст VK
/// подмешивает [`to_api_error`]), а необязательные поля (`captcha_sid`,
/// `redirect_uri`) не заполняются — из кода их не вывести.
///
/// Полная таблица (`retry` — колонка «авторетрай», см. [`retry_for`]):
///
/// | VK | HTTP | `code` | `retry_after_secs` | retry |
/// |----|------|--------|--------------------|-------|
/// | 1, 2, 3, 23 | 502 | `vk_error` | — | нет |
/// | 5 | 409 | `not_authorized` | — | нет |
/// | 6 | 429 | `flood_wait` | 1 | 1× после сна 1 с |
/// | 9 | 429 | `flood_wait` | 60 | НИКОГДА |
/// | 10 | 503 | `not_connected` | — | 1× |
/// | 14 | 409 | `captcha_required` | — | нет |
/// | 15 | 409 | `no_messages_scope` | — | нет |
/// | 17 | 409 | `validation_required` | — | нет |
/// | 18, 30, 113, 917, 936 | 404 | `unknown_dialog` | — | нет |
/// | 27, 28 | 409 | `not_authorized` | — | нет |
/// | 29 | 429 | `flood_wait` | 3600 | НИКОГДА |
/// | 100, 913, 914, 921 | 400 | `invalid_input` | — | нет |
/// | 900, 901, 902 | 409 | `send_forbidden` | — | нет |
/// | 909, 925 | 409 | `invalid_state` | — | нет |
/// | остальное | 502 | `vk_error` | — | нет |
///
/// Неизвестный код НЕ паникует и не «догадывается»: 502 `vk_error` — честный
/// ответ «VK отказал, и ни ввод, ни повтор этого не изменят».
pub fn classify(code: i64) -> ApiError {
    match code {
        // Общие отказы платформы: чинить нечего ни пользователю, ни нам.
        1 => ApiError::vk_error("VK reported an unknown internal error"),
        2 => ApiError::vk_error("the VK application is disabled"),
        3 => ApiError::vk_error("unknown VK method"),
        23 => ApiError::vk_error("this VK method is disabled"),

        CODE_AUTH_FAILED => ApiError::not_authorized("VK authorization failed: the token is invalid, expired or revoked"),
        // 27/28 — «ключ сообщества/приложения невалиден». Для нас это тот же
        // смысл, что и 5: имеющийся токен работать не будет.
        27 | 28 => ApiError::not_authorized("the VK access key is not valid"),

        CODE_TOO_MANY_PER_SECOND => ApiError::flood_wait(
            RETRY_AFTER_TOO_MANY_PER_SECOND,
            "VK: too many requests per second",
        ),
        CODE_FLOOD_CONTROL => ApiError::flood_wait(
            RETRY_AFTER_FLOOD_CONTROL,
            "VK flood control: repeat the request later",
        ),
        CODE_METHOD_QUOTA => ApiError::flood_wait(
            RETRY_AFTER_METHOD_QUOTA,
            "VK method quota reached: repeat the request later",
        ),

        CODE_INTERNAL_SERVER => ApiError::not_connected("VK internal server error"),

        // sid/img подставляет `to_api_error`: из одного кода их не вывести.
        CODE_CAPTCHA => ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "captcha_required",
            "VK requires a captcha",
        ),
        CODE_ACCESS_DENIED => ApiError::no_messages_scope(
            "the VK token has no `messages` permission (VK does not grant it to standard standalone apps)",
        ),
        // redirect_uri подставляет `to_api_error`.
        CODE_VALIDATION_REQUIRED => ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "validation_required",
            "VK requires user validation",
        ),

        18 => ApiError::unknown_dialog("the VK page is deleted or banned"),
        30 => ApiError::unknown_dialog("the VK profile is private"),
        113 => ApiError::unknown_dialog("invalid VK user id"),
        917 => ApiError::unknown_dialog("no access to this VK chat"),
        936 => ApiError::unknown_dialog("VK contact not found"),

        100 => ApiError::bad_input("VK rejected one of the parameters"),
        913 => ApiError::bad_input("too many forwarded messages"),
        914 => ApiError::bad_input("the message is too long"),
        921 => ApiError::bad_input("these messages cannot be forwarded"),

        900 => ApiError::send_forbidden("cannot write to a user from the blacklist"),
        901 => ApiError::send_forbidden("the user disallowed messages from this account"),
        902 => ApiError::send_forbidden("messages are forbidden by privacy settings"),

        909 => ApiError::invalid_state("the message is too old to be changed"),
        925 => ApiError::invalid_state("you are not an admin of this chat"),

        _ => ApiError::vk_error("VK returned an error"),
    }
}

/// Полное отображение [`VkError`] в контракт REST.
///
/// Здесь же подставляются поля, которых нет в [`classify`]: `captcha_sid` +
/// `captcha_img` (код 14) и `redirect_uri` (код 17) — они приходят из тела
/// ответа, а не из кода.
///
/// Текст VK подмешивается в `message` ПОСЛЕ канонического: пользователю нужен
/// понятный смысл, а разработчику — дословная формулировка VK. Секрета в этом
/// тексте нет: `request_params` вырезаны ещё в [`parse_envelope`].
pub fn to_api_error(err: &VkError) -> ApiError {
    match err {
        VkError::Transport { reason } => {
            ApiError::not_connected(format!("VK is unreachable: {reason}"))
        }
        VkError::Http { status } => {
            ApiError::not_connected(format!("api.vk.com replied with HTTP {status}"))
        }
        VkError::Decode { reason } => {
            ApiError::vk_error(format!("could not parse the VK response: {reason}"))
        }
        VkError::Api {
            code,
            msg,
            captcha_sid,
            captcha_img,
            redirect_uri,
        } => {
            let mut e = classify(*code);
            if !msg.is_empty() {
                e.body.message = format!("{} (VK error {code}: {msg})", e.body.message);
            }
            match e.body.code.as_str() {
                "captcha_required" => {
                    e.body.captcha_sid = captcha_sid.clone();
                    e.body.captcha_img = captcha_img.clone();
                }
                "validation_required" => e.body.redirect_uri = redirect_uri.clone(),
                _ => {}
            }
            e
        }
    }
}

// ---------------------------------------------------------------------------
// Политика ретраев.
// ---------------------------------------------------------------------------

/// Что делать с неудачей: повторять или нет, и как долго ждать.
///
/// Повтор ВСЕГДА не более одного: второй отказ — это уже состояние, а не
/// сетевая рябь, и цепочка повторов лишь оттягивает честный ответ
/// пользователю.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Не повторять. Ответ детерминирован — повтор ничего не изменит, а для
    /// кодов 9 и 29 ещё и продлевает бан (прямой аналог телеграмного
    /// `FLOOD_WAIT`).
    Never,
    /// Один повтор после «сетевой» паузы: [`NETWORK_BACKOFF`] + джиттер
    /// `0..=`[`NETWORK_BACKOFF_JITTER_MS`] мс.
    AfterNetworkBackoff,
    /// Один повтор ровно через [`FLOOD_RETRY_DELAY`].
    ///
    /// Код 6 означает, что НАШ троттлер промахнулся. Молча починить это
    /// честнее, чем показать пользователю 429 за собственную ошибку; при
    /// повторной 6 отдаём 429 и пишем `warn` — значит `rate_limit_rps` пора
    /// снижать.
    AfterOneSecond,
}

/// База «сетевой» паузы перед повтором.
pub const NETWORK_BACKOFF: Duration = Duration::from_millis(500);

/// Верхняя граница джиттера, миллисекунды.
///
/// Джиттер нужен, чтобы N параллельных запросов, упавших от одного обрыва, не
/// пошли на повтор в одну и ту же миллисекунду.
pub const NETWORK_BACKOFF_JITTER_MS: u64 = 250;

/// Пауза перед повтором кода 6 — ровно один «тик» лимита VK.
pub const FLOOD_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Политика повтора для конкретной ошибки.
///
/// - транспорт, HTTP 5xx, код 10 ⇒ [`Retry::AfterNetworkBackoff`];
/// - код 6 ⇒ [`Retry::AfterOneSecond`];
/// - всё остальное (включая коды 9 и 29!) ⇒ [`Retry::Never`].
///
/// HTTP 4xx повтора не заслуживает: это не рябь, а неверный адрес или прокси,
/// который так и будет отвечать.
pub fn retry_for(err: &VkError) -> Retry {
    match err {
        VkError::Transport { .. } => Retry::AfterNetworkBackoff,
        VkError::Http { status } if *status >= 500 => Retry::AfterNetworkBackoff,
        VkError::Http { .. } => Retry::Never,
        VkError::Decode { .. } => Retry::Never,
        VkError::Api { code, .. } => match *code {
            CODE_TOO_MANY_PER_SECOND => Retry::AfterOneSecond,
            CODE_INTERNAL_SERVER => Retry::AfterNetworkBackoff,
            _ => Retry::Never,
        },
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    const SECRET: &str = "vk1.a.THIS_IS_THE_ACCESS_TOKEN_0123456789";

    fn err_body(json: &str) -> VkError {
        parse_envelope(200, json.as_bytes()).unwrap_err()
    }

    // -----------------------------------------------------------------
    // Ф2.1 — пять веток parse_envelope.
    // -----------------------------------------------------------------

    #[test]
    fn parse_envelope_branch_1_non_200_is_transport() {
        for status in [301_u16, 403, 500, 502, 504] {
            let e = parse_envelope(status, br#"{"response":1}"#).unwrap_err();
            assert_eq!(e, VkError::Http { status });
            assert!(e.is_transport(), "HTTP {status} — транспортный класс");
        }
    }

    #[test]
    fn parse_envelope_branch_2_non_json_is_decode() {
        // Ровно то, чем отвечает captive portal или страница-заглушка VK.
        let e = parse_envelope(200, b"<html>hi</html>").unwrap_err();
        assert!(matches!(e, VkError::Decode { .. }), "{e:?}");
        // Пустое тело — тоже не JSON.
        assert!(matches!(
            parse_envelope(200, b"").unwrap_err(),
            VkError::Decode { .. }
        ));
    }

    #[test]
    fn parse_envelope_branch_3_error_key_wins() {
        let e = err_body(r#"{"error":{"error_code":15,"error_msg":"Access denied"}}"#);
        assert_eq!(e.code(), Some(15));
        assert_eq!(e.to_string(), "vk error 15: Access denied");

        // Оба ключа сразу: приоритет у ошибки, иначе отказ проехал бы как
        // успех с урезанными данными.
        let e = err_body(r#"{"response":[],"error":{"error_code":6,"error_msg":"Too many"}}"#);
        assert_eq!(e.code(), Some(6));
    }

    #[test]
    fn parse_envelope_branch_4_response_is_returned_as_is() {
        let v = parse_envelope(200, br#"{"response":{"count":2,"items":[1,2]}}"#).unwrap();
        assert_eq!(v["count"], 2);
        assert_eq!(v["items"][1], 2);

        // Скалярный `response` (messages.send отдаёт голое число) — тоже успех.
        assert_eq!(parse_envelope(200, br#"{"response":42}"#).unwrap(), 42);
        // …и `null`, и `false` — это НЕ отсутствие ключа.
        assert!(parse_envelope(200, br#"{"response":null}"#).unwrap().is_null());
        assert_eq!(parse_envelope(200, br#"{"response":false}"#).unwrap(), false);
    }

    #[test]
    fn parse_envelope_branch_5_json_without_both_keys_is_decode() {
        let e = parse_envelope(200, br#"{"items":[]}"#).unwrap_err();
        assert!(matches!(e, VkError::Decode { .. }), "{e:?}");
        // Валидный JSON, но не объект.
        assert!(matches!(
            parse_envelope(200, b"[1,2,3]").unwrap_err(),
            VkError::Decode { .. }
        ));
    }

    // -----------------------------------------------------------------
    // Ф2.1 — ГЛАВНЫЙ ИНВАРИАНТ ФАЗЫ.
    // -----------------------------------------------------------------

    /// `request_params` не должны доехать ни до `Debug`, ни до `Display`, ни
    /// до тела HTTP-ответа. Мутация «вернуть `request_params` в `VkError`»
    /// обязана красить именно этот тест.
    #[test]
    fn error_request_params_are_stripped() {
        let bodies = [
            // 1. Каноническая форма VK: список пар key/value.
            format!(
                r#"{{"error":{{"error_code":5,"error_msg":"User authorization failed",
                   "request_params":[{{"key":"access_token","value":"{SECRET}"}},
                                     {{"key":"v","value":"5.199"}}]}}}}"#
            ),
            // 2. Секрет лежит под ДРУГИМ ключом — маска по списку имён его бы
            //    не поймала, а мы выбрасываем поле целиком.
            format!(
                r#"{{"error":{{"error_code":100,"error_msg":"bad param",
                   "request_params":[{{"key":"key","value":"{SECRET}"}}]}}}}"#
            ),
            // 3. Вложенный объект вместо плоского списка (форма не
            //    документирована и может измениться в любой момент).
            format!(
                r#"{{"error":{{"error_code":6,"error_msg":"Too many requests",
                   "request_params":{{"auth":{{"access_token":"{SECRET}"}}}}}}}}"#
            ),
            // 4. Секрет в глубине массива объектов.
            format!(
                r#"{{"error":{{"error_code":9,"error_msg":"Flood control",
                   "request_params":[{{"nested":[{{"v":"{SECRET}"}}]}}]}}}}"#
            ),
        ];

        for body in bodies {
            let err = parse_envelope(200, body.as_bytes()).unwrap_err();

            let dbg = format!("{err:?}");
            let disp = err.to_string();
            assert!(!dbg.contains(SECRET), "секрет в Debug: {dbg}");
            assert!(!disp.contains(SECRET), "секрет в Display: {disp}");
            assert!(!dbg.contains("THIS_IS_THE"), "часть секрета в Debug: {dbg}");
            assert!(!dbg.contains("request_params"), "поле уцелело: {dbg}");

            // И дальше по конвейеру: тело HTTP-ответа тоже чисто.
            let api = to_api_error(&err);
            let rendered = serde_json::to_string(&api.body).unwrap();
            assert!(!rendered.contains(SECRET), "секрет в теле ответа: {rendered}");
            assert!(!rendered.contains("request_params"), "{rendered}");
        }
    }

    /// Тот же инвариант для upload-сервера: он видел ровно те же параметры.
    #[test]
    fn upload_error_request_params_are_stripped() {
        let body = format!(
            r#"{{"error":{{"error_code":1,"error_msg":"upload failed",
               "request_params":[{{"key":"access_token","value":"{SECRET}"}}]}}}}"#
        );
        let err = parse_upload_response(200, body.as_bytes()).unwrap_err();
        assert!(!format!("{err:?}").contains(SECRET), "{err:?}");
    }

    /// Успешный ответ upload-сервера — голый объект без конверта.
    #[test]
    fn upload_response_has_no_envelope() {
        let v = parse_upload_response(200, br#"{"server":123,"photo":"[]","hash":"abc"}"#).unwrap();
        assert_eq!(v["server"], 123);
        assert_eq!(v["hash"], "abc");
        // …а `parse_envelope` на том же теле обязан отказать — это разные
        // протоколы, и путать их нельзя.
        assert!(parse_envelope(200, br#"{"server":123}"#).is_err());
    }

    /// Ошибка VK приезжает с HTTP **200** — принципиальное отличие от
    /// Telegram. Тест держит это на виду: если кто-то начнёт судить об
    /// успехе по статусу, он свалится здесь.
    #[test]
    fn vk_errors_arrive_with_http_200() {
        let e = parse_envelope(200, br#"{"error":{"error_code":5,"error_msg":"nope"}}"#)
            .unwrap_err();
        assert_eq!(e.code(), Some(5));
        assert!(!e.is_transport(), "код 5 — это отказ VK, а не сети");
    }

    /// Захваченные из тела поля captcha и валидации.
    #[test]
    fn captcha_and_validation_fields_survive_the_strip() {
        let e = err_body(
            r#"{"error":{"error_code":14,"error_msg":"Captcha needed",
                "captcha_sid":"239123","captcha_img":"https://api.vk.com/captcha.php?sid=239123",
                "request_params":[{"key":"access_token","value":"x"}]}}"#,
        );
        assert_eq!(
            e.captcha(),
            Some(("239123", "https://api.vk.com/captcha.php?sid=239123"))
        );

        // `captcha_sid` числом — VK отдаёт его и так, и так.
        let e = err_body(r#"{"error":{"error_code":14,"captcha_sid":239123,"captcha_img":"u"}}"#);
        assert_eq!(e.captcha(), Some(("239123", "u")));

        let e = err_body(
            r#"{"error":{"error_code":17,"error_msg":"Validation required",
                "redirect_uri":"https://m.vk.com/login?act=security_check"}}"#,
        );
        let api = to_api_error(&e);
        assert_eq!(
            api.body.redirect_uri.as_deref(),
            Some("https://m.vk.com/login?act=security_check")
        );
    }

    /// Форма oauth (`{"error":"invalid_client"}`) не должна ронять разбор.
    #[test]
    fn string_shaped_error_degrades_to_vk_error() {
        let e = err_body(r#"{"error":"invalid_client","error_description":"bad app"}"#);
        assert_eq!(e.code(), Some(0));
        assert_eq!(to_api_error(&e).status, StatusCode::BAD_GATEWAY);
    }

    // -----------------------------------------------------------------
    // Ф2.2 — ПОЛНАЯ таблица кодов.
    // -----------------------------------------------------------------

    /// Аналог телеграмного `full_error_contract_table`, но по КОДАМ VK:
    /// код → HTTP-статус → `code` в теле → `retry_after_secs` → авторетрай.
    ///
    /// Это единственное место, где зафиксирована вся таблица. Любое
    /// расхождение с документацией фазы обязано валить тест, а не тихо менять
    /// поведение вкладки.
    #[test]
    fn full_vk_code_contract_table() {
        // (код VK, HTTP, code, retry_after_secs, повторяем ли)
        let table: &[(i64, StatusCode, &str, Option<u64>, Retry)] = &[
            (1, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (2, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (3, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (5, StatusCode::CONFLICT, "not_authorized", None, Retry::Never),
            (
                6,
                StatusCode::TOO_MANY_REQUESTS,
                "flood_wait",
                Some(1),
                Retry::AfterOneSecond,
            ),
            (
                9,
                StatusCode::TOO_MANY_REQUESTS,
                "flood_wait",
                Some(60),
                Retry::Never,
            ),
            (
                10,
                StatusCode::SERVICE_UNAVAILABLE,
                "not_connected",
                None,
                Retry::AfterNetworkBackoff,
            ),
            (
                14,
                StatusCode::CONFLICT,
                "captcha_required",
                None,
                Retry::Never,
            ),
            (
                15,
                StatusCode::CONFLICT,
                "no_messages_scope",
                None,
                Retry::Never,
            ),
            (
                17,
                StatusCode::CONFLICT,
                "validation_required",
                None,
                Retry::Never,
            ),
            (18, StatusCode::NOT_FOUND, "unknown_dialog", None, Retry::Never),
            (23, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (27, StatusCode::CONFLICT, "not_authorized", None, Retry::Never),
            (28, StatusCode::CONFLICT, "not_authorized", None, Retry::Never),
            (
                29,
                StatusCode::TOO_MANY_REQUESTS,
                "flood_wait",
                Some(3600),
                Retry::Never,
            ),
            (30, StatusCode::NOT_FOUND, "unknown_dialog", None, Retry::Never),
            (100, StatusCode::BAD_REQUEST, "invalid_input", None, Retry::Never),
            (113, StatusCode::NOT_FOUND, "unknown_dialog", None, Retry::Never),
            (900, StatusCode::CONFLICT, "send_forbidden", None, Retry::Never),
            (901, StatusCode::CONFLICT, "send_forbidden", None, Retry::Never),
            (902, StatusCode::CONFLICT, "send_forbidden", None, Retry::Never),
            (909, StatusCode::CONFLICT, "invalid_state", None, Retry::Never),
            (913, StatusCode::BAD_REQUEST, "invalid_input", None, Retry::Never),
            (914, StatusCode::BAD_REQUEST, "invalid_input", None, Retry::Never),
            (917, StatusCode::NOT_FOUND, "unknown_dialog", None, Retry::Never),
            (921, StatusCode::BAD_REQUEST, "invalid_input", None, Retry::Never),
            (925, StatusCode::CONFLICT, "invalid_state", None, Retry::Never),
            (936, StatusCode::NOT_FOUND, "unknown_dialog", None, Retry::Never),
            // Неизвестный код: 502 без паники и без «догадок».
            (4242, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (0, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (-7, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
            (i64::MAX, StatusCode::BAD_GATEWAY, "vk_error", None, Retry::Never),
        ];

        for (code, status, name, retry_after, retry) in table {
            let api = classify(*code);
            assert_eq!(api.status, *status, "HTTP-статус кода {code}");
            assert_eq!(api.body.code, *name, "code кода {code}");
            assert_eq!(
                api.body.retry_after_secs, *retry_after,
                "retry_after_secs кода {code}"
            );
            assert!(
                !api.body.message.is_empty(),
                "код {code}: сообщение обязано быть непустым"
            );

            let err = VkError::Api {
                code: *code,
                msg: String::new(),
                captcha_sid: None,
                captcha_img: None,
                redirect_uri: None,
            };
            assert_eq!(retry_for(&err), *retry, "политика повтора кода {code}");
        }
    }

    /// Транспортные строки той же таблицы — они не проходят через
    /// [`classify`], но обязаны давать те же 503 и тот же один повтор.
    /// Мутация «отдать 500 вместо 503» краснит именно этот тест.
    #[test]
    fn transport_rows_of_the_contract_table() {
        let transport = VkError::Transport {
            reason: "connection refused".into(),
        };
        assert_eq!(to_api_error(&transport).status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(to_api_error(&transport).body.code, "not_connected");
        assert_eq!(retry_for(&transport), Retry::AfterNetworkBackoff);

        for status in [500_u16, 502, 503, 504] {
            let e = VkError::Http { status };
            assert_eq!(
                to_api_error(&e).status,
                StatusCode::SERVICE_UNAVAILABLE,
                "HTTP {status}"
            );
            assert_eq!(to_api_error(&e).body.code, "not_connected");
            assert_eq!(retry_for(&e), Retry::AfterNetworkBackoff, "HTTP {status}");
        }

        // 4xx — это не рябь: прокси так и будет отвечать.
        for status in [400_u16, 403, 404, 429] {
            let e = VkError::Http { status };
            assert_eq!(to_api_error(&e).body.code, "not_connected", "HTTP {status}");
            assert_eq!(retry_for(&e), Retry::Never, "HTTP {status}");
        }

        let decode = VkError::Decode {
            reason: "expected value".into(),
        };
        assert_eq!(to_api_error(&decode).status, StatusCode::BAD_GATEWAY);
        assert_eq!(retry_for(&decode), Retry::Never);
    }

    /// Коды 9 и 29 не повторяются НИКОГДА — автоповтор продлевает бан.
    /// Мутация «поменять код 9 на ретраить» краснит этот тест.
    #[test]
    fn flood_codes_are_never_retried() {
        for code in [CODE_FLOOD_CONTROL, CODE_METHOD_QUOTA] {
            let err = VkError::Api {
                code,
                msg: "flood".into(),
                captcha_sid: None,
                captcha_img: None,
                redirect_uri: None,
            };
            assert_eq!(retry_for(&err), Retry::Never, "код {code}");
            assert!(classify(code).body.retry_after_secs.is_some(), "код {code}");
        }
        // …а код 6 — повторяется, ровно один раз и ровно через секунду.
        let six = VkError::Api {
            code: CODE_TOO_MANY_PER_SECOND,
            msg: String::new(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        assert_eq!(retry_for(&six), Retry::AfterOneSecond);
        assert_eq!(FLOOD_RETRY_DELAY, Duration::from_secs(1));
    }

    /// Текст VK доезжает до пользователя, но не вытесняет канонический смысл.
    #[test]
    fn vk_message_is_appended_not_replaced() {
        let err = VkError::Api {
            code: 901,
            msg: "Can't send messages for users without permission".into(),
            captcha_sid: None,
            captcha_img: None,
            redirect_uri: None,
        };
        let api = to_api_error(&err);
        assert_eq!(api.body.code, "send_forbidden");
        assert!(
            api.body.message.contains("disallowed messages"),
            "канонический смысл обязан остаться: {}",
            api.body.message
        );
        assert!(api.body.message.contains("VK error 901"), "{}", api.body.message);
    }

    /// Необязательные поля появляются ТОЛЬКО там, где обещаны: лишний
    /// `retry_after_secs` заставил бы UI показать обратный отсчёт на ровном
    /// месте, а лишний `captcha_sid` — открыть пустой оверлей.
    #[test]
    fn optional_fields_appear_only_where_promised() {
        for code in [1, 5, 10, 15, 18, 100, 900, 909, 4242] {
            let b = classify(code).body;
            assert!(b.retry_after_secs.is_none(), "код {code}");
            assert!(b.captcha_sid.is_none(), "код {code}");
            assert!(b.captcha_img.is_none(), "код {code}");
            assert!(b.redirect_uri.is_none(), "код {code}");
        }
        // Захваченные из тела поля не появляются у чужих кодов.
        let e = VkError::Api {
            code: 5,
            msg: String::new(),
            captcha_sid: Some("239".into()),
            captcha_img: Some("u".into()),
            redirect_uri: Some("https://x".into()),
        };
        let b = to_api_error(&e).body;
        assert!(b.captcha_sid.is_none(), "captcha у кода 5: {b:?}");
        assert!(b.redirect_uri.is_none(), "redirect у кода 5: {b:?}");
    }
}
