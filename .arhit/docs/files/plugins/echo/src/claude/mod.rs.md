# plugins/echo/src/claude/mod.rs

Фасад Echo над раннером Claude CLI. С Фазы 0 фичи «Анализ чата через Claude в VK» модуль СХЛОПНУТ до re-export'а: сам раннер живёт в общем крейте workspace forge-claude-runner (plugins/claude-runner), см. страницу forge-claude-runner.

Содержимое модуля: pub mod prompt_builder; + pub use forge_claude_runner::events; + pub use forge_claude_runner::{ClaudeEvent, ClaudeRunner, RunId, RunRequest, RunResult, Usage, DEFAULT_RUN_TIMEOUT_SECS}.

Смысл фасада — нулевой дифф у вызывающих: все исторические пути crate::claude::* (в том числе crate::claude::events::Usage) резолвятся по-прежнему, ни один из ~22 файлов Echo, использующих раннер, при выносе не менялся. Если ради вызова придётся править путь — значит re-export неполный.

prompt_builder.rs ОСТАЁТСЯ в Echo и в общий крейт не переезжает: он echo-специфичен (знает про HostApi, память и capture-pane tmux).

Осторожно: crate::config::DEFAULT_RUN_TIMEOUT_SECS (дефолт конфига Echo, 300 c) и forge_claude_runner::DEFAULT_RUN_TIMEOUT_SECS (дефолт самого раннера, тоже 300 c) — две разные константы с одинаковым значением. Конфиг Echo прокидывается в раннер через ClaudeRunner::with_run_timeout.
