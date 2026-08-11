//! Анализ чата через Claude.
//!
//! # Ядро вынесено в общий крейт
//!
//! Всё, что не зависит от Telegram, живёт в `forge-chat-analysis` и
//! переиспользуется VK и Bitrix'ом:
//!
//! - [`prompts`] — пер-чатовое хранилище (заметка, последний разбор, тред);
//!   путь файла (`~/.forge/telegram-chat-prompts.json`, см.
//!   [`crate::config::TelegramConfig::prompts_path`]) передаётся параметром;
//! - [`prompt`] — теговая сборка запроса и системная инструкция;
//! - [`hub`] — реестр активных run'ов поверх `forge-claude-runner`.
//!
//! Здесь остаётся ровно то, что говорит с Telegram: [`context`] — выгрузка
//! истории диалога через `client.iter_messages` и рендер транскрипта из
//! [`crate::dto::MessageDto`].
//!
//! Развязка с ядром одна: кадры оно вещает не в broadcast Telegram, а через
//! трейт `AnalysisSink`, который плагин реализует в
//! [`crate::ws::protocol::TgAnalysisSink`]. Ошибки ядро отдаёт своим
//! `AnalysisError`, а перевод в `ApiError` (то есть выбор HTTP-кода) делает
//! [`crate::routes::analysis`] — крейт про HTTP не знает.
//!
//! Переэкспорт модулей под путями `crate::analysis::*` — то же соглашение,
//! что у VK: вызывающий код не должен знать, лежит ли кусок фичи в плагине
//! или в общем крейте.

pub mod context;

pub use forge_chat_analysis::{hub, prompt, prompts};

pub use forge_chat_analysis::{
    ActiveRun, AnalysisError, AnalysisHub, AnalysisSink, StartOutcome, StartRequest,
    DEFAULT_ANALYSIS_TIMEOUT_SECS, DELTA_FLUSH_CHARS, DELTA_FLUSH_MS, KIND_ANALYZE, KIND_ASK,
    KIND_REFRESH, MAX_PARALLEL_ANALYSIS, REASON_CANCELLED, REASON_ERROR, REASON_TIMEOUT,
};
