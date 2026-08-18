//! Единый тип ошибки плагина и её машиночитаемый код.
//!
//! Чистый модуль: `axum` здесь не импортируется, преобразование в HTTP-ответ
//! живёт в [`crate::routes`]. Так репозитории и пайплайн могут возвращать
//! осмысленную ошибку, не зная про HTTP вообще.
//!
//! # Лестница ошибок
//!
//! Порядок проверок в роутах зафиксирован планом и обязан быть именно таким:
//!
//! ```text
//! готовность бинарей (409 *_unavailable)
//!   → слот (409 busy)
//!     → резолв встречи (404)
//!       → допустимость перехода (409 invalid_state)
//!         → разбор параметров (400)
//! ```
//!
//! Причина не в эстетике. Экстрактор `Query` у axum отработал бы ДО тела
//! хендлера и отдал 400 раньше, чем мы успели сказать «whisper не установлен»
//! — и пользователь чинил бы не то. Поэтому query разбирается вручную внутри
//! хендлера, после проверки зависимостей.

use serde::Serialize;

/// Ошибка операции плагина.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetError {
    /// Встречи с таким идентификатором нет.
    NotFound,
    /// Переход запрещён таблицей [`crate::status::can_transition`].
    InvalidState {
        from: &'static str,
        to: &'static str,
    },
    /// Все слоты записи заняты. Отвечается НЕМЕДЛЕННО, без очереди: встреча
    /// идёт прямо сейчас, и ожидание слота означало бы, что бот войдёт, когда
    /// всё уже кончилось.
    Busy,
    /// Нет внешнего бинаря или модели. Код вида `whisper_unavailable`.
    DepUnavailable(&'static str),
    /// Параметры запроса не разбираются.
    BadRequest(String),
    /// Запрошенный диапазон байт лежит за пределами файла.
    ///
    /// Отдельно от [`BadRequest`](MeetError::BadRequest), потому что ответ на
    /// него обязан нести `Content-Range: bytes */LEN` — то есть длину файла, а
    /// её надо где-то хранить до момента сборки ответа.
    RangeNotSatisfiable {
        /// Фактическая длина файла, байт.
        len: u64,
    },
    /// База ещё не открыта: `init` синхронен и файлов не трогает, БД
    /// открывается в `spawn_workers`. Между этими моментами роуты обязаны
    /// отвечать внятно, а не паниковать.
    DbUnavailable,
    /// Ошибка SQLite.
    Db(String),
    /// Всё остальное.
    Internal(String),
}

impl MeetError {
    /// Машиночитаемый код — то, по чему фронт различает случаи. Человеческий
    /// текст меняется, код нет.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidState { .. } => "invalid_state",
            Self::Busy => "busy",
            Self::DepUnavailable(code) => code,
            Self::BadRequest(_) => "bad_request",
            Self::RangeNotSatisfiable { .. } => "range_not_satisfiable",
            Self::DbUnavailable => "db_unavailable",
            Self::Db(_) => "db_error",
            Self::Internal(_) => "internal",
        }
    }

    /// Код HTTP-статуса. Возвращается числом, чтобы модуль остался свободным
    /// от `axum` и `http`.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::BadRequest(_) => 400,
            Self::InvalidState { .. } | Self::Busy | Self::DepUnavailable(_) => 409,
            Self::RangeNotSatisfiable { .. } => 416,
            Self::DbUnavailable => 503,
            Self::Db(_) | Self::Internal(_) => 500,
        }
    }

    /// Текст для пользователя.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "встреча не найдена".to_string(),
            Self::InvalidState { from, to } => {
                format!("переход {from} → {to} недопустим")
            }
            Self::Busy => "все слоты записи заняты".to_string(),
            Self::DepUnavailable(code) => format!("не готов внешний компонент: {code}"),
            Self::BadRequest(m) => m.clone(),
            Self::RangeNotSatisfiable { len } => {
                format!("запрошенный диапазон за пределами файла длиной {len} байт")
            }
            Self::DbUnavailable => "хранилище ещё не открыто".to_string(),
            Self::Db(m) => format!("ошибка хранилища: {m}"),
            Self::Internal(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for MeetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for MeetError {}

impl From<tokio_rusqlite::Error> for MeetError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<rusqlite::Error> for MeetError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

/// Тело ответа об ошибке. Форма одна на все роуты плагина.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub error: &'static str,
    pub message: String,
}

impl From<&MeetError> for ErrorBody {
    fn from(e: &MeetError) -> Self {
        Self {
            error: e.code(),
            message: e.message(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Лестница ошибок держится на том, что коды статусов упорядочены именно
    /// так: 409 у зависимостей и слота раньше, чем 404 у резолва, и 400 в
    /// самом конце.
    #[test]
    fn the_error_ladder_has_the_documented_status_codes() {
        assert_eq!(MeetError::DepUnavailable("whisper_unavailable").http_status(), 409);
        assert_eq!(MeetError::Busy.http_status(), 409);
        assert_eq!(MeetError::NotFound.http_status(), 404);
        assert_eq!(
            MeetError::InvalidState { from: "ready", to: "recording" }.http_status(),
            409
        );
        assert_eq!(MeetError::BadRequest("x".into()).http_status(), 400);
        assert_eq!(MeetError::RangeNotSatisfiable { len: 10 }.http_status(), 416);
    }

    #[test]
    fn dependency_codes_pass_through_unchanged() {
        // Фронт различает случаи по коду, поэтому `whisper_unavailable` не
        // имеет права превратиться в общее `dep_unavailable`.
        assert_eq!(
            MeetError::DepUnavailable("whisper_unavailable").code(),
            "whisper_unavailable"
        );
        assert_eq!(
            MeetError::DepUnavailable("chrome_unavailable").code(),
            "chrome_unavailable"
        );
    }

    #[test]
    fn every_error_serialises_with_a_code_and_a_message() {
        for e in [
            MeetError::NotFound,
            MeetError::Busy,
            MeetError::DbUnavailable,
            MeetError::InvalidState { from: "ready", to: "queued" },
            MeetError::BadRequest("плохой диапазон".into()),
            MeetError::RangeNotSatisfiable { len: 42 },
            MeetError::Db("disk full".into()),
            MeetError::Internal("boom".into()),
        ] {
            let body = ErrorBody::from(&e);
            assert!(!body.error.is_empty());
            assert!(!body.message.is_empty(), "{e}");
            serde_json::to_string(&body).unwrap();
        }
    }
}
