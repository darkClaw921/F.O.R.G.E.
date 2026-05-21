# main

Точка входа devforge (tmux-web/src/main.rs). #[tokio::main] async fn main() -> anyhow::Result<()>.

## --help / --version

В самом начале main() (до tracing_subscriber/init и любых тяжёлых операций) обрабатывается лёгкий флаг --help / -h: std::env::args().any(|a| a == "--help" || a == "-h") печатает usage (имя бинаря, дефолтный порт 3000, ссылка на репо) и std::process::exit(0). Это нужно для Homebrew test do блока.

## Bootstrap (после --help-handler)

После remove-projects-concept (Phase 4) ProjectStore удалён из bootstrap. Sequence:
1. tracing_subscriber.
2. CliArgs::parse + ServerConfig::load (~/.config/forge/server_config.json).
3. AuthToken / RemotesStore (только в remote_mode).
4. ~/.config/forge/{todos.json,notifier.json,user_settings.json} → TodoStore, NotifierConfigStore, UserSettingsStore.
5. Init active_path_tx с std::env::current_dir() в качестве начального значения.
6. broadcast/watch каналы: tasks_tx (TaskEvent), todos_tx (TodoEvent).
7. AttentionState::new() + notifier::start(...).
8. AppState собирается со всеми полями.
9. Фоновые task'и: tasks_watcher::run_watcher(active_path_rx, tasks_tx), attention::watcher_loop(state.attention).
10. Echo plugin: forge_echo::init + register_routes ДО auth_middleware + spawn_workers.
11. Регистрация axum-роутов (включая static_embed::serve_static как fallback).
12. axum::serve на SocketAddr 0.0.0.0:3000 (или порт из ServerConfig).
