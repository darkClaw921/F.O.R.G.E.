//! Единый контракт ошибок REST-поверхности плагина.
//!
//! Тело ответа всегда одной формы — фронту не нужно угадывать, что
//! пришло:
//!
//! ```json
//! {"error":{"code":"not_authorized","message":"Telegram account is not linked yet"}}
//! {"error":{"code":"flood_wait","message":"too many attempts","retry_after_secs":42}}
//! ```
//!
//! | HTTP | code | когда |
//! |------|------|-------|
//! | 400 | `invalid_input`  | Telegram отверг введённые данные (код, пароль, телефон) |
//! | 409 | `not_authorized` | аккаунт не привязан / сессия отозвана |
//! | 409 | `invalid_state`  | операция невозможна в текущем состоянии логина; либо заняты все слоты анализа чата |
//! | 409 | `claude_unavailable` | бинаря Claude CLI нет по пути `claude_cli_path` |
//! | 500 | `storage_failed` | локальный файл заметок/разборов не записался |
//! | 404 | `unknown_dialog` | диалога нет или он недоступен |
//! | 404 | `unknown_message`| сообщения нет (удалено / чужой `msg_id`) |
//! | 404 | `no_media`       | у сообщения нет запрошенного медиа |
//! | 411 | `length_required`| `POST /upload` без `Content-Length` |
//! | 413 | `too_large`      | файл больше `max_upload_bytes` |
//! | 429 | `flood_wait`     | Telegram попросил подождать `retry_after_secs` |
//! | 502 | `telegram_error` | Telegram ответил ошибкой, которую не исправит ни ввод, ни реконнект |
//! | 503 | `not_connected`  | MTProto-соединение отсутствует |
//! | 504 | `bot_timeout`    | бот не ответил на нажатие кнопки за отведённое Telegram время |
//!
//! `429 flood_wait` НИКОГДА не ретраится автоматически: FLOOD_WAIT на
//! логине измеряется часами, и повторные попытки только продлевают
//! блокировку. UI обязан показать обратный отсчёт.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Тело ошибки (внутренний объект).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    /// Машинный код (см. таблицу в шапке модуля).
    pub code: String,
    /// Человекочитаемое пояснение — можно показывать пользователю.
    pub message: String,
    /// Сколько секунд ждать перед повтором (только для `flood_wait`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

/// Обёртка `{"error": {...}}` — ровно то, что уходит на провод.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEnvelope {
    /// Сама ошибка.
    pub error: ApiErrorBody,
}

/// Ошибка REST-хендлера. Реализует `IntoResponse`, поэтому хендлеры
/// возвращают `Result<Json<T>, ApiError>` и не собирают ответы руками.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP-статус.
    pub status: StatusCode,
    /// Тело ошибки.
    pub body: ApiErrorBody,
}

impl ApiError {
    /// Произвольная ошибка с явными статусом и кодом.
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                code: code.to_string(),
                message: message.into(),
                retry_after_secs: None,
            },
        }
    }

    /// 400 — Telegram отверг введённые пользователем данные.
    ///
    /// Отличается от `invalid_state` тем, что состояние сохраняется:
    /// пользователь исправляет ввод и повторяет тот же шаг.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_input", message)
    }

    /// 409 — аккаунт Telegram не привязан (или сессия перестала быть валидной).
    pub fn not_authorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "not_authorized", message)
    }

    /// 409 — операция невозможна в текущем состоянии логина.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "invalid_state", message)
    }

    /// 404 — диалог не найден.
    pub fn unknown_dialog(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_dialog", message)
    }

    /// 404 — сообщения нет: удалено, отредактировано до неузнаваемости или
    /// `msg_id` принадлежит другому диалогу.
    ///
    /// Отдельный код от [`ApiError::unknown_dialog`] нужен нажатию кнопки
    /// бота: «диалога нет» заставляет фронт перерисовать список диалогов, а
    /// «сообщения нет» — всего лишь погасить устаревшую клавиатуру в одной
    /// строке лога.
    pub fn unknown_message(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_message", message)
    }

    /// 404 — у сообщения нет запрошенного медиа.
    pub fn no_media(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "no_media", message)
    }

    /// 502 — Telegram ответил ошибкой, которую клиент исправить не может.
    ///
    /// Это НЕ `not_connected`: соединение живо, и повтор того же запроса
    /// ничего не изменит. И не `invalid_input`: ввод пользователя ни при
    /// чём. Фронт по этому коду показывает ошибку и НЕ предлагает ни
    /// «переподключиться», ни «исправить данные».
    pub fn telegram_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "telegram_error", message)
    }

    /// 504 — бот не ответил на нажатие кнопки.
    ///
    /// Единственная ошибка, где виноват не Telegram и не мы, а сторонний
    /// бот: он не прислал `answerCallbackQuery` за отведённое время. Повтор
    /// осмыслен (в отличие от `telegram_error`), поэтому статус
    /// «шлюз не дождался», а не «шлюз сломался».
    pub fn bot_timeout(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, "bot_timeout", message)
    }

    /// 411 — загрузка файла пришла без `Content-Length`.
    ///
    /// Отдельный код (а не `invalid_input`), потому что клиент по нему
    /// понимает конкретную поправку: тело уже не читалось, размер надо
    /// прислать заголовком и повторить запрос.
    pub fn length_required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LENGTH_REQUIRED, "length_required", message)
    }

    /// 413 — файл больше разрешённого `max_upload_bytes`.
    ///
    /// Тело при этом НЕ читается: отказ выносится по `Content-Length`, иначе
    /// клиент зря залил бы гигабайт ради `413`.
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    }

    /// 429 — Telegram просит подождать. Ретрай — только по решению
    /// пользователя, автоматических повторов быть не должно.
    pub fn flood_wait(secs: u64, message: impl Into<String>) -> Self {
        let mut e = Self::new(StatusCode::TOO_MANY_REQUESTS, "flood_wait", message);
        e.body.retry_after_secs = Some(secs);
        e
    }

    /// 503 — MTProto-соединения нет.
    pub fn not_connected(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "not_connected", message)
    }

    /// 409 — бинаря Claude CLI нет по настроенному пути.
    ///
    /// `409`, а не `503`: сервер жив и полностью работоспособен, не хватает
    /// внешнего инструмента, и починка — это действие пользователя, а не
    /// ожидание. Путь НАЗЫВАЕТСЯ в сообщении намеренно: без него единственный
    /// способ понять, где именно CLI искали, — читать логи сервера. Формировать
    /// текст здесь, а не в ядре анализа, — решение плагина:
    /// `forge-chat-analysis` общий на три мессенджера и про `telegram.toml`
    /// ничего не знает.
    ///
    /// Код и статус совпадают с VK и Bitrix один в один: фронтовое ядро
    /// (`js/messenger/analysis-core.js`) разбирает `err.code` одинаково для
    /// всех трёх панелей.
    pub fn claude_unavailable(path: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "claude_unavailable",
            format!(
                "Claude CLI не найден: {path}; установите CLI или укажите claude_cli_path \
                 в ~/.config/forge/telegram.toml"
            ),
        )
    }

    /// 500 — локальный файл заметок/разборов не записался.
    ///
    /// Сообщение собирает ВЫЗЫВАЮЩИЙ и обязан класть в него только причину
    /// io-ошибки: ни текста заметки, ни текста разбора, ни переписки в нём
    /// быть не может — ответ уходит в браузер и попадает в логи прокси.
    pub fn storage_failed(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_failed",
            message,
        )
    }
}

/// Единственный маппинг ошибок логина в HTTP.
///
/// Держится здесь, а не в хендлерах, чтобы контракт нельзя было
/// «разъехать» между четырьмя роутами логина.
impl From<crate::login::LoginError> for ApiError {
    fn from(e: crate::login::LoginError) -> Self {
        use crate::login::LoginError as L;
        match e {
            L::InvalidState(m) => Self::invalid_state(m),
            L::BadInput(m) => Self::bad_input(m),
            L::FloodWait { secs, message } => Self::flood_wait(secs, message),
            L::SignUpRequired => Self::invalid_state(L::SignUpRequired.to_string()),
            L::NotConnected(m) => Self::not_connected(m),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (
            self.status,
            Json(ApiErrorEnvelope {
                error: self.body.clone(),
            }),
        )
            .into_response();
        // Retry-After — стандартный заголовок; клиенты и прокси понимают его
        // без знания нашего JSON-контракта.
        if let Some(secs) = self.body.retry_after_secs {
            if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(err: ApiError) -> (StatusCode, serde_json::Value) {
        let status = err.status;
        let json = serde_json::to_value(ApiErrorEnvelope { error: err.body }).unwrap();
        (status, json)
    }

    #[test]
    fn codes_map_to_expected_statuses() {
        let cases = [
            (ApiError::not_authorized("x"), StatusCode::CONFLICT, "not_authorized"),
            (ApiError::invalid_state("x"), StatusCode::CONFLICT, "invalid_state"),
            (ApiError::unknown_dialog("x"), StatusCode::NOT_FOUND, "unknown_dialog"),
            (
                ApiError::unknown_message("x"),
                StatusCode::NOT_FOUND,
                "unknown_message",
            ),
            (ApiError::no_media("x"), StatusCode::NOT_FOUND, "no_media"),
            (
                ApiError::telegram_error("x"),
                StatusCode::BAD_GATEWAY,
                "telegram_error",
            ),
            (
                ApiError::not_connected("x"),
                StatusCode::SERVICE_UNAVAILABLE,
                "not_connected",
            ),
            (
                ApiError::bot_timeout("x"),
                StatusCode::GATEWAY_TIMEOUT,
                "bot_timeout",
            ),
            (
                ApiError::length_required("x"),
                StatusCode::LENGTH_REQUIRED,
                "length_required",
            ),
            (
                ApiError::too_large("x"),
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
            ),
        ];
        for (err, status, code) in cases {
            let (got_status, json) = body_of(err);
            assert_eq!(got_status, status);
            assert_eq!(json["error"]["code"], code);
            assert!(
                json["error"].get("retry_after_secs").is_none(),
                "retry_after_secs не должен появляться без flood_wait: {json}"
            );
        }
    }

    #[test]
    fn flood_wait_carries_retry_after() {
        let (status, json) = body_of(ApiError::flood_wait(42, "wait"));
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["error"]["code"], "flood_wait");
        assert_eq!(json["error"]["retry_after_secs"], 42);
    }

    #[test]
    fn login_errors_map_to_the_contract() {
        use crate::login::LoginError as L;

        let cases = [
            (
                L::InvalidState("x".into()),
                StatusCode::CONFLICT,
                "invalid_state",
            ),
            (L::BadInput("x".into()), StatusCode::BAD_REQUEST, "invalid_input"),
            (
                L::NotConnected("x".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "not_connected",
            ),
            (L::SignUpRequired, StatusCode::CONFLICT, "invalid_state"),
        ];
        for (err, status, code) in cases {
            let api: ApiError = err.into();
            assert_eq!(api.status, status);
            assert_eq!(api.body.code, code);
            assert!(!api.body.message.is_empty());
            assert!(api.body.retry_after_secs.is_none());
        }

        let api: ApiError = L::flood(65).into();
        assert_eq!(api.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(api.body.code, "flood_wait");
        assert_eq!(api.body.retry_after_secs, Some(65));
    }

    /// ПОЛНАЯ таблица контракта: конструктор → HTTP-статус → код в теле.
    ///
    /// Это единственное место, где зафиксирован весь список кодов. Фронт
    /// (js/telegram/api.js, upload.js) принимает решения ИМЕННО по `code`, а
    /// не по статусу: 409 бывает и `not_authorized`, и `invalid_state`, и
    /// реакция на них разная. Поэтому переименование кода обязано валить
    /// тест, а не тихо ломать вкладку.
    ///
    /// Проверяется настоящий `IntoResponse`, а не только структура: заголовок
    /// `Content-Type` и точные байты тела — часть контракта.
    #[tokio::test]
    async fn full_error_contract_table() {
        let table: Vec<(ApiError, StatusCode, &str)> = vec![
            (
                ApiError::bad_input("x"),
                StatusCode::BAD_REQUEST,
                "invalid_input",
            ),
            (
                ApiError::not_authorized("x"),
                StatusCode::CONFLICT,
                "not_authorized",
            ),
            (
                ApiError::invalid_state("x"),
                StatusCode::CONFLICT,
                "invalid_state",
            ),
            (
                ApiError::unknown_dialog("x"),
                StatusCode::NOT_FOUND,
                "unknown_dialog",
            ),
            (
                ApiError::unknown_message("x"),
                StatusCode::NOT_FOUND,
                "unknown_message",
            ),
            (ApiError::no_media("x"), StatusCode::NOT_FOUND, "no_media"),
            (
                ApiError::length_required("x"),
                StatusCode::LENGTH_REQUIRED,
                "length_required",
            ),
            (
                ApiError::too_large("x"),
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
            ),
            (
                ApiError::flood_wait(7, "x"),
                StatusCode::TOO_MANY_REQUESTS,
                "flood_wait",
            ),
            (
                ApiError::telegram_error("x"),
                StatusCode::BAD_GATEWAY,
                "telegram_error",
            ),
            (
                ApiError::not_connected("x"),
                StatusCode::SERVICE_UNAVAILABLE,
                "not_connected",
            ),
            (
                ApiError::bot_timeout("x"),
                StatusCode::GATEWAY_TIMEOUT,
                "bot_timeout",
            ),
        ];

        let mut seen: Vec<String> = Vec::new();
        for (err, status, code) in table {
            let expects_retry = code == "flood_wait";
            let resp = err.into_response();
            assert_eq!(resp.status(), status, "статус для {code}");
            let ct = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(ct.starts_with("application/json"), "{code}: {ct}");

            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(json["error"]["code"], code);
            assert!(
                json["error"]["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{code}: сообщение обязано быть непустым"
            );
            assert_eq!(
                json["error"].get("retry_after_secs").is_some(),
                expects_retry,
                "{code}: retry_after_secs присутствует только у flood_wait"
            );
            // Никаких полей сверх контракта: фронт разбирает ровно три.
            let obj = json["error"].as_object().unwrap();
            assert!(obj.len() <= 3, "{code}: лишние поля в теле {json}");
            seen.push(code.to_string());
        }

        seen.sort();
        assert_eq!(
            seen,
            vec![
                "bot_timeout",
                "flood_wait",
                "invalid_input",
                "invalid_state",
                "length_required",
                "no_media",
                "not_authorized",
                "not_connected",
                "telegram_error",
                "too_large",
                "unknown_dialog",
                "unknown_message",
            ],
            "список кодов разошёлся с таблицей в шапке модуля"
        );
    }

    /// Сторож компилятора для [`crate::login::LoginError`].
    ///
    /// Новый вариант ошибки логина ломает сборку тестов, пока автор не решит,
    /// в какой код контракта он отображается. Без этого новая ошибка тихо
    /// уехала бы во `From` через `_ =>` (которого сейчас нет) или в панику.
    #[test]
    fn every_login_error_variant_is_mapped() {
        use crate::login::LoginError as L;

        fn code_of(e: &L) -> &'static str {
            match e {
                L::InvalidState(_) => "invalid_state",
                L::BadInput(_) => "invalid_input",
                L::FloodWait { .. } => "flood_wait",
                L::SignUpRequired => "invalid_state",
                L::NotConnected(_) => "not_connected",
            }
        }

        let all = [
            L::InvalidState("x".into()),
            L::BadInput("x".into()),
            L::flood(1),
            L::SignUpRequired,
            L::NotConnected("x".into()),
        ];
        for e in all {
            let expected = code_of(&e);
            let api: ApiError = e.into();
            assert_eq!(api.body.code, expected);
        }
    }

    /// `Retry-After` — часть контракта наравне с JSON: стандартные клиенты
    /// и прокси читают именно заголовок. Он обязан появляться у `flood_wait`
    /// (и совпадать с телом) и НЕ появляться у остальных ошибок — иначе
    /// прокси начнёт «ждать» там, где ждать нечего.
    #[tokio::test]
    async fn retry_after_header_mirrors_the_body_and_only_for_flood_wait() {
        let resp = ApiError::flood_wait(42, "wait").into_response();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("42")
        );

        for err in [
            ApiError::bad_input("x"),
            ApiError::not_authorized("x"),
            ApiError::not_connected("x"),
        ] {
            let code = err.body.code.clone();
            let resp = err.into_response();
            assert!(
                resp.headers().get(axum::http::header::RETRY_AFTER).is_none(),
                "{code}: Retry-After допустим только у flood_wait"
            );
        }
    }

    /// Границы `retry_after_secs`: Telegram реально присылает `FLOOD_WAIT_0`
    /// (нулевое ожидание), а верхняя граница не должна ни паниковать, ни
    /// терять точность в JSON (u64 сериализуется числом, не строкой).
    #[tokio::test]
    async fn flood_wait_boundary_values_survive_serialization() {
        // 0 секунд — поле и заголовок присутствуют, а не пропадают как «пустые».
        let resp = ApiError::flood_wait(0, "now").into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("0")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["retry_after_secs"], 0);

        // u64::MAX — вырожденный, но не должен ломать ни заголовок, ни JSON.
        let resp = ApiError::flood_wait(u64::MAX, "forever").into_response();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("18446744073709551615")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["retry_after_secs"].as_u64(), Some(u64::MAX));
    }

    /// В `message` попадает пользовательский ввод (телефон, имена RPC-ошибок,
    /// имена файлов): кавычки, переводы строк, emoji и RTL-override обязаны
    /// доехать до клиента без порчи JSON. Ловит регрессию «собрали тело
    /// руками через format!» вместо serde.
    #[tokio::test]
    async fn message_with_hostile_characters_stays_valid_json() {
        let hostile = "код \"1\"\nстрока\t😀 \u{202E}RTL <script>alert(1)</script>";
        let resp = ApiError::bad_input(hostile).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["message"].as_str(), Some(hostile));
    }

    #[test]
    fn envelope_round_trips() {
        let env = ApiErrorEnvelope {
            error: ApiErrorBody {
                code: "not_authorized".into(),
                message: "nope".into(),
                retry_after_secs: None,
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert_eq!(
            json,
            r#"{"error":{"code":"not_authorized","message":"nope"}}"#
        );
        let back: ApiErrorEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }
}
