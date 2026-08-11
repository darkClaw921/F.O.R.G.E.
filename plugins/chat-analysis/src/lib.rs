//! Общее ядро фичи «Анализ чата через Claude» для плагинов-мессенджеров.
//!
//! Фича сделана и принята в `forge-vk`; Telegram и Bitrix получают её ровно из
//! этого крейта, а не копией. Здесь живёт всё, что НЕ зависит от конкретного
//! мессенджера:
//!
//! - [`prompts`] — пер-чатовое хранилище: заметка пользователя, последний
//!   разбор и тред вопросов, один JSON-файл на плагин (путь — параметр);
//! - [`prompt`] — теговая сборка запроса к модели и системная инструкция;
//! - [`hub`] — реестр активных run'ов поверх [`forge_claude_runner`]: дедуп по
//!   беседе, потолок параллелизма, коалесцинг дельт, сохранение результата;
//! - [`atomic`] — атомарная запись приватного файла (`tmp` → `fsync` →
//!   `rename` → `0600`);
//! - [`sink`] — [`AnalysisSink`], через который ядро вещает кадры, ничего не
//!   зная о WS-протоколе плагина;
//! - [`error`] — [`AnalysisError`], который плагин переводит в свой конверт.
//!
//! # Что ОСТАЁТСЯ в плагине
//!
//! Выгрузка истории и рендер транскрипта (у каждого мессенджера свой API и
//! свой DTO), HTTP-роуты с лестницей проверок, варианты WS-кадров и реализация
//! [`AnalysisSink`], поля конфига и состояния. Крейт про HTTP не знает вовсе:
//! ни axum, ни кодов ответа в нём нет.

pub mod atomic;
pub mod error;
pub mod hub;
pub mod prompt;
pub mod prompts;
pub mod sink;
pub mod testing;
pub mod text;
pub mod transcript;

#[cfg(test)]
mod testlog;

pub use error::AnalysisError;
pub use hub::{
    ActiveRun, AnalysisHub, StartOutcome, StartRequest, DEFAULT_ANALYSIS_TIMEOUT_SECS,
    DELTA_FLUSH_CHARS, DELTA_FLUSH_MS, KIND_ANALYZE, KIND_ASK, KIND_REFRESH,
    MAX_PARALLEL_ANALYSIS, REASON_CANCELLED, REASON_ERROR, REASON_TIMEOUT,
};
pub use sink::AnalysisSink;
pub use transcript::Transcript;

/// Общий `tracing`-target всех модулей крейта.
///
/// Один на крейт, а не `forge_vk` / `forge_telegram` по вызывающему: события
/// ядра приходят из ЕГО тасок (пампер переживает HTTP-запрос), и подписать их
/// именем плагина значило бы соврать о том, кто их пишет. Фильтр
/// `RUST_LOG=chat_analysis=debug` включает анализ во всех трёх мессенджерах
/// разом — это и требуется при разборе жалобы «разбор не сохранился».
pub const LOG_TARGET: &str = "chat_analysis";
