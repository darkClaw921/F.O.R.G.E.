# /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./plugins/echo-host-api/src/lib.rs

echo-host-api — мини-крейт plugin boundary для Echo. Содержит trait HostApi (#[async_trait], Send + Sync) с методами: list_sessions, capture_pane_full, list_projects, active_project_id, auth_token. DTO: SessionInfo { name, windows, panes }, ProjectInfo { id, name, path }. Без зависимостей на tmux-web/axum/db — это даёт echo плагину компилироваться независимо и тестироваться с мок-хостом. Конкретная реализация HostApi живёт в tmux-web/src/echo_host.rs (EchoHostAdapter).
