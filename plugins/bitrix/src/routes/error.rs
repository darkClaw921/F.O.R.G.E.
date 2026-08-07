//! Единый контракт ошибок REST-поверхности плагина.
//!
//! Тело ответа всегда одной формы — фронту не нужно угадывать, что пришло:
//!
//! ```json
//! {"error":{"code":"not_authorized","message":"Bitrix24 webhook is not configured yet"}}
//! {"error":{"code":"flood_wait","message":"portal is throttling","retry_after_secs":60}}
//! ```
//!
//! | HTTP | code | когда |
//! |------|------|-------|
//! | 400 | `invalid_input`       | `DIALOG_ID_EMPTY`, `MESSAGE_EMPTY`, `PARAMS_ERROR`, `*_ERROR`, либо наш разбор отверг параметры |
//! | 404 | `unknown_dialog`      | диалога нет либо он недоступен |
//! | 404 | `unknown_message`     | сообщения нет |
//! | 404 | `unknown_task`        | задачи нет либо она недоступна |
//! | 404 | `no_media`            | у вложения нет запрошенного представления |
//! | 409 | `not_authorized`      | `INVALID_CREDENTIALS`, `NO_AUTH_FOUND` — вебхука нет или он отозван |
//! | 409 | `scope_missing`       | `insufficient_scope` — у вебхука не отмечены нужные права |
//! | 409 | `unsupported_auth`    | `WRONG_AUTH_TYPE` — метод недоступен входящему вебхуку в принципе |
//! | 409 | `invalid_state`       | `CANT_EDIT_MESSAGE` и прочие «сейчас нельзя» |
//! | 409 | `send_forbidden`      | `ERROR_NO_ACCESS`, `ACCESS_DENIED` — писать в этот чат нельзя |
//! | 411 | `length_required`     | `POST /upload` без `Content-Length` |
//! | 413 | `too_large`           | файл больше `max_upload_bytes` |
//! | 429 | `flood_wait`          | `QUERY_LIMIT_EXCEEDED` (интенсивность) или `OPERATION_TIME_LIMIT` (ресурсоёмкость) |
//! | 501 | `method_unavailable`  | `ERROR_METHOD_NOT_FOUND` — метода нет на этой редакции портала |
//! | 502 | `bitrix_error`        | портал ответил ошибкой, которую не исправит ни ввод, ни повтор |
//! | 503 | `not_connected`       | транспортный отказ, пустое тело при HTTP 200, обрыв TCP |
//!
//! ## Две ловушки платформы, отражённые в таблице
//!
//! 1. **Пустое тело при HTTP 200 — это троттлинг**, а не битый JSON. Портал
//!    при превышении интенсивности иногда просто обрывает ответ, не отдав ни
//!    строчки. Такой случай обязан стать `not_connected` (503, повторяемо), а
//!    не `bitrix_error` (502, «чините ввод»).
//! 2. **`OPERATION_TIME_LIMIT` блокирует КОНКРЕТНЫЙ метод**, а не весь
//!    портал, и автоповтор его только продлевает. Поэтому `flood_wait` из
//!    этой ветки несёт `retry_after_secs` из `operating_reset_at` и НЕ
//!    ретраится транспортом — ждать обязан пользователь.
//!
//! Заголовков `X-RateLimit-*` и `Retry-After` портал не присылает, поэтому
//! `retry_after_secs` мы считаем сами и дублируем стандартным заголовком
//! `Retry-After` — прокси и клиенты понимают его без знания нашего JSON.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Тело ошибки (внутренний объект).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

    /// 400 — параметры отвергнуты (нами или порталом).
    ///
    /// Сюда же попадает текст длиннее 20 000 символов (Р12): портал молча
    /// режет такое сообщение, и «успешная» отправка обрубка хуже честного
    /// отказа.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_input", message)
    }

    /// 409 — вебхук не задан либо портал его больше не принимает.
    pub fn not_authorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "not_authorized", message)
    }

    /// 409 — у вебхука нет нужных прав (`insufficient_scope`).
    ///
    /// Отдельный код, а не `not_authorized`: вебхук исправен, и предлагать
    /// «вставьте новый» бессмысленно — надо пересоздать его, отметив «Чат и
    /// уведомления» и «Задачи».
    pub fn scope_missing(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "scope_missing", message)
    }

    /// 409 — метод недоступен входящему вебхуку в принципе
    /// (`WRONG_AUTH_TYPE`): он требует OAuth-приложения.
    ///
    /// Отдельный код нужен, чтобы UI объяснил ограничение словами, а не
    /// показал «ошибка сервера» и не выглядел поломкой.
    pub fn unsupported_auth(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "unsupported_auth", message)
    }

    /// 409 — операция невозможна в текущем состоянии (`CANT_EDIT_MESSAGE`,
    /// действие задачи, отсутствующее в матрице `action`).
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "invalid_state", message)
    }

    /// 409 — писать в этот чат нельзя (`ERROR_NO_ACCESS`, `ACCESS_DENIED`).
    ///
    /// Отдельный код от `bitrix_error`, потому что реакция UI другая: поле
    /// ввода блокируется с объяснением, а «повторить» не предлагается.
    pub fn send_forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "send_forbidden", message)
    }

    /// 404 — диалога нет либо он недоступен.
    pub fn unknown_dialog(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_dialog", message)
    }

    /// 404 — сообщения нет.
    ///
    /// Отдельный код от [`ApiError::unknown_dialog`]: «диалога нет» заставляет
    /// фронт перерисовать список, «сообщения нет» — всего лишь убрать одну
    /// строку ленты.
    pub fn unknown_message(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_message", message)
    }

    /// 404 — задачи нет либо она недоступна.
    pub fn unknown_task(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "unknown_task", message)
    }

    /// 404 — у вложения нет запрошенного представления (например, превью у
    /// документа).
    pub fn no_media(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "no_media", message)
    }

    /// 411 — загрузка файла пришла без `Content-Length`.
    ///
    /// Отдельный код (а не `invalid_input`), потому что клиент по нему
    /// понимает конкретную поправку: тело ещё не читалось, размер надо
    /// прислать заголовком и повторить запрос.
    pub fn length_required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LENGTH_REQUIRED, "length_required", message)
    }

    /// 413 — файл больше разрешённого `max_upload_bytes` (Р7).
    ///
    /// Тело при этом НЕ читается: отказ выносится по `Content-Length`, иначе
    /// клиент зря залил бы 25 МБ ради `413`.
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    }

    /// 429 — портал просит подождать.
    ///
    /// Два источника с разной природой: `QUERY_LIMIT_EXCEEDED` (интенсивность,
    /// ~1 секунда) и `OPERATION_TIME_LIMIT` (ресурсоёмкость конкретного
    /// метода, до `operating_reset_at`). Оба доезжают до пользователя одним
    /// кодом, потому что реакция UI одинакова — обратный отсчёт.
    pub fn flood_wait(secs: u64, message: impl Into<String>) -> Self {
        let mut e = Self::new(StatusCode::TOO_MANY_REQUESTS, "flood_wait", message);
        e.body.retry_after_secs = Some(secs);
        e
    }

    /// 501 — метода нет на этой редакции портала
    /// (`ERROR_METHOD_NOT_FOUND`).
    ///
    /// Не 502: портал исправен и отвечает по протоколу — просто такой
    /// возможности у него нет. Фронт по этому коду прячет функцию, а не
    /// показывает «сбой».
    pub fn method_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_IMPLEMENTED, "method_unavailable", message)
    }

    /// 502 — портал ответил ошибкой, которую клиент исправить не может.
    ///
    /// Это НЕ `not_connected`: сеть жива, и повтор того же запроса ничего не
    /// изменит. И не `invalid_input`: ввод пользователя ни при чём.
    pub fn bitrix_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "bitrix_error", message)
    }

    /// 503 — транспортный отказ (см. [`crate::state::ConnHealth`]).
    ///
    /// Сюда же попадает ПУСТОЕ ТЕЛО при HTTP 200 — так портал выглядит, когда
    /// душит интенсивность: это повторяемая ситуация, а не битый JSON.
    pub fn not_connected(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "not_connected", message)
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

    /// ПОЛНАЯ таблица контракта: конструктор → HTTP-статус → код в теле.
    ///
    /// Это единственное место, где зафиксирован весь список кодов. Фронт
    /// принимает решения ИМЕННО по `code`, а не по статусу: 409 бывает пяти
    /// разных смыслов, и реакция на них разная. Поэтому переименование кода
    /// обязано валить тест, а не тихо менять поведение панели.
    #[test]
    fn the_error_table_is_frozen() {
        let table: [(ApiError, u16, &str); 16] = [
            (ApiError::bad_input("x"), 400, "invalid_input"),
            (ApiError::unknown_dialog("x"), 404, "unknown_dialog"),
            (ApiError::unknown_message("x"), 404, "unknown_message"),
            (ApiError::unknown_task("x"), 404, "unknown_task"),
            (ApiError::no_media("x"), 404, "no_media"),
            (ApiError::not_authorized("x"), 409, "not_authorized"),
            (ApiError::scope_missing("x"), 409, "scope_missing"),
            (ApiError::unsupported_auth("x"), 409, "unsupported_auth"),
            (ApiError::invalid_state("x"), 409, "invalid_state"),
            (ApiError::send_forbidden("x"), 409, "send_forbidden"),
            (ApiError::length_required("x"), 411, "length_required"),
            (ApiError::too_large("x"), 413, "too_large"),
            (ApiError::flood_wait(1, "x"), 429, "flood_wait"),
            (ApiError::method_unavailable("x"), 501, "method_unavailable"),
            (ApiError::bitrix_error("x"), 502, "bitrix_error"),
            (ApiError::not_connected("x"), 503, "not_connected"),
        ];
        for (err, status, code) in table {
            assert_eq!(err.status.as_u16(), status, "код {code}");
            assert_eq!(err.body.code, code);
        }
    }

    #[tokio::test]
    async fn the_envelope_shape_is_stable() {
        let resp = ApiError::bad_input("bad").into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "invalid_input");
        assert_eq!(v["error"]["message"], "bad");
        // Необязательное поле не сериализуется вовсе — старый фронт-разбор
        // (`code` + `message`) продолжает работать без правок.
        assert!(v["error"].get("retry_after_secs").is_none(), "{v}");
    }

    #[tokio::test]
    async fn flood_wait_sets_both_the_field_and_the_header() {
        let resp = ApiError::flood_wait(42, "wait").into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("42")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["retry_after_secs"], 42);
    }

    #[tokio::test]
    async fn errors_without_retry_after_do_not_set_the_header() {
        let resp = ApiError::not_connected("down").into_response();
        assert!(resp.headers().get(axum::http::header::RETRY_AFTER).is_none());
    }
}
