//! Единый контракт ошибок `/api/tracker/*`.
//!
//! Тело всегда одной формы — `{"error":{"code":"…","message":"…"}}`. Форма
//! совпадает с остальными плагинами (`forge-vk`, `forge-bitrix`): фронт
//! разбирает ошибку одним общим помощником, и новая панель не приносит с собой
//! ещё один формат.
//!
//! # Три кода и граница между 400 и 409
//!
//! | код HTTP | `code` | когда |
//! |---|---|---|
//! | 400 | `bad_request` | запрос НЕВАЛИДЕН сам по себе: пустой заголовок, неизвестный статус, дата, которая не разбирается, конец отрезка не позже начала |
//! | 404 | `not_found` | адресован объект, которого нет: задача, список, пункт, веха, отрезок |
//! | 409 | `conflict` | запрос валиден, но состояние стора его не принимает: например, вторая попытка сделать отрезок активным при уже идущем таймере той же задачи |
//! | 500 | `internal` | беда с `tracker.json`: нет прав, диск, сериализация |
//!
//! Граница проходит по вопросу «изменится ли ответ, если состояние стора
//! станет другим». Перевёрнутый отрезок (`stopped_at <= started_at`) не станет
//! законным ни при каком состоянии — это 400, и именно так его проверяют
//! роуты, ДО обращения к стору. Второй активный таймер той же задачи, наоборот,
//! станет законным, стоит остановить первый — это 409.
//!
//! Стор той разницы не знает: он честно отдаёт
//! [`TrackerError::Conflict`] в обоих случаях, и [`From`] переводит его в 409.
//! Роуты снимают перевёрнутые интервалы раньше, поэтому до стора такой запрос
//! доходит только как страховка от забытой проверки.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::TrackerError;

/// Ошибка HTTP-слоя трекера.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// Код ответа.
    pub status: StatusCode,
    /// Машиночитаемый код для фронта.
    pub code: &'static str,
    /// Человекочитаемое пояснение.
    pub message: String,
}

impl ApiError {
    /// 400: запрос невалиден сам по себе.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    /// 404: объекта с таким идентификатором нет.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }

    /// 409: состояние стора не принимает валидный запрос.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: message.into(),
        }
    }

    /// 500: беда с хранилищем.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        });
        (self.status, Json(body)).into_response()
    }
}

impl From<TrackerError> for ApiError {
    fn from(e: TrackerError) -> Self {
        match e {
            TrackerError::NotFound(msg) => ApiError::not_found(msg),
            TrackerError::Conflict(msg) => ApiError::conflict(msg),
            TrackerError::Io(msg) | TrackerError::Json(msg) => {
                // Текст беды с файлом уезжает в лог целиком, а клиенту
                // достаётся общая формулировка: путь к файлу пользователя и
                // текст ошибки файловой системы наружу не отдаются.
                tracing::error!(target: "forge_tracker", error = %msg, "tracker store failure");
                ApiError::internal("tracker storage failure")
            }
        }
    }
}

/// Короткий alias результата роутов.
pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_not_found_becomes_404() {
        let err: ApiError = TrackerError::NotFound("task x".into()).into();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn store_conflict_becomes_409() {
        let err: ApiError = TrackerError::Conflict("busy".into()).into();
        assert_eq!(err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn io_error_does_not_leak_the_path() {
        let err: ApiError = TrackerError::Io("/Users/kate/.config/forge/tracker.json: EACCES".into())
            .into();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err.message.contains("kate"), "{}", err.message);
    }
}
