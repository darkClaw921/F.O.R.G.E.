# tmux-web/src/main.rs

Главный entry point tmux-web HTTP-сервера. Содержит axum Router с регистрацией всех HTTP/WS routes (themes, projects, tasks, todos, ws/attach, ws/lazygit, ws/tasks, ws/todos), AppState, обработчики GET/POST для проектов и тем. После Phase 3 forge-gda удалены: REST git API routes, handler-функции (get_git_status, get_git_log, post_git_stage, post_git_unstage, post_git_commit), DTO структуры (LogQuery, PathsReq, CommitReq) — git-функционал переехал на WS endpoint /ws/lazygit (lazygit TUI в браузере).
