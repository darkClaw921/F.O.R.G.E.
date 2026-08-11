//! Claude CLI integration плагина Echo.
//!
//! Сам раннер живёт в общем крейте workspace
//! [`forge_claude_runner`](../../../claude-runner/src/lib.rs): плагины
//! изолированы друг от друга, поэтому `forge-vk` не может зависеть от
//! `forge-echo`, а второй копии процессного кода (spawn / kill / таймаут /
//! семафор / парсер stream-json) в репозитории быть не должно.
//!
//! Этот модуль остаётся фасадом: все исторические пути `crate::claude::*`
//! (включая `crate::claude::events::*`) резолвятся по-прежнему, поэтому
//! вызывающие файлы Echo при выносе не менялись.
//!
//! Sub-модули:
//! - [`prompt_builder`] — сборка prompt'а с capture-pane + memories. Остаётся
//!   здесь: он echo-специфичен (знает про `HostApi`, память и tmux-панель) и в
//!   общий крейт не переезжает.
//! - [`events`] — реэкспорт `forge_claude_runner::events` (NDJSON-парсер).
//!
//! Главный фасад — [`ClaudeRunner`].

pub mod prompt_builder;

/// NDJSON-парсер стрима `claude -p --output-format stream-json`.
/// Реэкспорт модуля общего крейта — путь `crate::claude::events::…` сохранён.
pub use forge_claude_runner::events;

// Весь публичный набор раннера. Прежде эти имена жили прямо здесь, поэтому
// реэкспорт обязан быть полным — иначе где-то в Echo пришлось бы править путь,
// а смысл выноса именно в нулевом диффе у вызывающих.
//
// `DEFAULT_RUN_TIMEOUT_SECS` раннера — это дефолт самого крейта; дефолт
// конфига Echo (`crate::config::DEFAULT_RUN_TIMEOUT_SECS`) остался отдельной
// константой с тем же значением 300 и продолжает прокидываться через
// `ClaudeRunner::with_run_timeout`.
pub use forge_claude_runner::{
    ClaudeEvent, ClaudeRunner, RunId, RunRequest, RunResult, Usage, DEFAULT_RUN_TIMEOUT_SECS,
};
