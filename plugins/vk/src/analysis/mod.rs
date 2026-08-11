//! Анализ чата через Claude.
//!
//! # Ядро вынесено в общий крейт
//!
//! Всё, что не зависит от API ВКонтакте, живёт в `forge-chat-analysis` и
//! переиспользуется Telegram'ом и Bitrix'ом:
//!
//! - [`prompts`] — пер-чатовое хранилище (заметка, последний разбор, тред);
//!   путь файла (`~/.forge/vk-chat-prompts.json`, см.
//!   [`crate::config::VkConfig::prompts_path`]) передаётся параметром;
//! - [`prompt`] — теговая сборка запроса и системная инструкция;
//! - [`hub`] — реестр активных run'ов поверх `forge-claude-runner`.
//!
//! Здесь остаётся ровно то, что говорит с ВКонтакте: [`context`] (Ф2) —
//! выгрузка истории беседы и рендер транскрипта из `MessageDto`.
//!
//! Развязка с ядром одна: кадры оно вещает не в broadcast VK, а через трейт
//! `AnalysisSink`, который плагин реализует в
//! [`crate::ws::protocol::VkAnalysisSink`]. Ошибки ядро отдаёт своим
//! `AnalysisError`, а перевод в `ApiError` (то есть выбор HTTP-кода) делает
//! [`crate::routes::analysis`] — крейт про HTTP не знает.
//!
//! Модули переэкспортированы под прежними путями (`crate::analysis::prompts`
//! и т. д.) намеренно: вынос — чистый рефакторинг, и ни один вызывающий, ни
//! один тест не должен был поменять ни строки, кроме этих.
//!
//! Общее правило подмодулей: НИ ОДИН из них не тянет `AppState` хоста и не
//! знает про `forge-echo` — раннер живёт в общем крейте `forge-claude-runner`
//! именно поэтому (плагины изолированы друг от друга).

pub mod context;

pub use forge_chat_analysis::{hub, prompt, prompts};

pub use forge_chat_analysis::{
    ActiveRun, AnalysisError, AnalysisHub, AnalysisSink, StartOutcome, StartRequest,
    DEFAULT_ANALYSIS_TIMEOUT_SECS, DELTA_FLUSH_CHARS, DELTA_FLUSH_MS, KIND_ANALYZE, KIND_ASK,
    KIND_REFRESH, MAX_PARALLEL_ANALYSIS, REASON_CANCELLED, REASON_ERROR, REASON_TIMEOUT,
};
