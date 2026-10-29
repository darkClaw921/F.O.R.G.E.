//! Анализ чата через Claude.
//!
//! # Ядро вынесено в общий крейт
//!
//! Всё, что не зависит от Bitrix24, живёт в `forge-chat-analysis` и
//! переиспользуется VK и Telegram'ом:
//!
//! - [`prompts`] — пер-чатовое хранилище (заметка, последний разбор, тред);
//!   путь файла (`~/.forge/bitrix-chat-prompts.json`, см.
//!   [`crate::config::BitrixConfig::prompts_path`]) передаётся параметром;
//! - [`prompt`] — теговая сборка запроса и системная инструкция;
//! - [`hub`] — реестр активных run'ов поверх `forge-claude-runner`.
//!
//! Здесь остаётся ровно то, что говорит с ПОРТАЛОМ: [`context`] — выгрузка
//! истории диалога через [`crate::routes::dialogs::load_page`] и рендер
//! транскрипта из [`crate::dto::MessageDto`].
//!
//! Развязка с ядром одна: кадры оно вещает не в broadcast Bitrix, а через
//! трейт `AnalysisSink`, который плагин реализует в
//! [`crate::ws::protocol::B24AnalysisSink`]. Ошибки ядро отдаёт своим
//! `AnalysisError`, а перевод в `ApiError` (то есть выбор HTTP-кода) делает
//! [`crate::routes::analysis`] — крейт про HTTP не знает.
//!
//! Переэкспорт модулей под путями `crate::analysis::*` — то же соглашение,
//! что у VK и Telegram: вызывающий код не должен знать, лежит ли кусок фичи в
//! плагине или в общем крейте.
//!
//! # Три отличия Bitrix, из-за которых [`context`] не копия VK-шного
//!
//! 1. **Потолок страницы платформы — 50 сообщений**
//!    ([`crate::api::methods::MESSAGES_MAX_LIMIT`]) при бюджете 2 rps, который
//!    делят воркер событий и фолбэк-синхронизация. Отсюда своя лестница
//!    глубин ([`context::DEPTH_CHOICES`]) — см. её обоснование в шапке
//!    [`context`].
//! 2. **Дата приезжает строкой ATOM**, а не unix-секундами: `stamp` и `day`
//!    считаются разбором через `chrono`, а не арифметикой над числом.
//! 3. **Цитаты живут ВНУТРИ BB-кодов** — полей `reply_to`/`fwd` у портала нет
//!    вовсе (они всегда `None`), и разбор цитаты пишется поверх
//!    [`crate::bbcode`].

pub mod context;

pub use forge_chat_analysis::{hub, prompt, prompts};

pub use forge_chat_analysis::{
    resolve_transport, ActiveRun, AnalysisError, AnalysisHub, AnalysisSink, StartOutcome,
    StartRequest, DEFAULT_ANALYSIS_TIMEOUT_SECS, DELTA_FLUSH_CHARS, DELTA_FLUSH_MS, KIND_ANALYZE,
    KIND_ASK, KIND_REFRESH, MAX_PARALLEL_ANALYSIS, REASON_CANCELLED, REASON_ERROR, REASON_TIMEOUT,
};
