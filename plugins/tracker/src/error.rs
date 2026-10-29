//! Типизированные ошибки крейта `forge-tracker`.
//!
//! Крейт листовой и его будет вызывать HTTP-слой фазы 2, поэтому ошибка обязана
//! нести не только текст, но и КЛАСС проблемы: по нему роут выбирает код
//! ответа. Отсюда четыре варианта, а не один `anyhow`:
//!
//! * [`TrackerError::Io`] и [`TrackerError::Json`] — беда с хранилищем
//!   (`~/.config/forge/tracker.json`): нет прав, диск, битый файл. Это `500`.
//! * [`TrackerError::NotFound`] — ссылка на несуществующий объект (задача,
//!   список, пункт, веха, отрезок). Это `404`.
//! * [`TrackerError::Conflict`] — запрос синтаксически верен, но нарушает
//!   инвариант модели: например, ручной отрезок с `stopped_at <= started_at`.
//!   Это `409`.
//!
//! Загрузка стора НЕ использует эти варианты как фатальные: битый файл там
//! логируется через `warn` и даёт пустой стор, чтобы сервер поднялся (см.
//! [`crate::store::TrackerStore::load`]).

/// Ошибка операции трекера.
#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    /// Файловая ошибка при чтении или записи стора.
    #[error("tracker io error: {0}")]
    Io(String),

    /// Ошибка сериализации/десериализации JSON стора.
    #[error("tracker json error: {0}")]
    Json(String),

    /// Объект с таким `id` в сторе не найден.
    #[error("tracker object not found: {0}")]
    NotFound(String),

    /// Запрос нарушает инвариант модели.
    #[error("tracker conflict: {0}")]
    Conflict(String),
}

impl From<std::io::Error> for TrackerError {
    fn from(e: std::io::Error) -> Self {
        TrackerError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for TrackerError {
    fn from(e: serde_json::Error) -> Self {
        TrackerError::Json(e.to_string())
    }
}

/// Короткий alias результата операций трекера.
pub type Result<T> = std::result::Result<T, TrackerError>;
