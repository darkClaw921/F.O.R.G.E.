# tmux-web/src/main.rs

Главный entry point tmux-web HTTP-сервера. Содержит axum Router с регистрацией всех HTTP/WS routes, AppState, обработчики REST-эндпоинтов, конфигурацию tracing.

## AppState

Делится между всеми handler'ами (axum extractor State<AppState>). Включает PathBuf для конфигурации проектов/тем, ThemeStore, ProjectStore, TaskWatcher handles, NotifyHandle, RwLock<RemoteStore>, флаг remote_mode и пр.

## HTTP routes

- /healthz — bootstrap-метрики и origin-инфо.
- /api/sessions [GET, POST] + /:name [DELETE] — управление tmux-сессиями.
- /api/tasks [GET, POST] + /:id [PATCH, DELETE] + /:id/reopen [POST] — beads-задачи.
- /api/projects [GET, POST] + /:id [DELETE] + /:id/settings [PATCH] + /active [POST] + /init [POST].
- /api/todos [GET, POST] + /:id [PATCH, DELETE] + /:id/promote [POST].
- /api/themes [GET] + /active [GET, PATCH] + /custom [POST].
- /api/remote-servers, /api/remote-servers/:id, /api/remote-servers/:id/healthz — remote-mode.

## WebSocket routes

- /ws/attach (ws::attach) — tmux-session bridge через PTY.
- /ws/lazygit (ws::lazygit_attach) — lazygit TUI bridge.
- /ws/lazydocker (ws::lazydocker_attach) — lazydocker TUI bridge (Phase 1, forge-ddyl).
- /ws/telescope (ws::telescope_attach) — television (tv) fuzzy-finder TUI bridge (Phase 1, forge-ddyl).
- /ws/tasks (ws_tasks::tasks_ws) — broadcast beads updates.
- /ws/todos (ws_todos::todos_ws) — broadcast todos updates.

Lazydocker и telescope-handler'ы — зеркала lazygit_attach: query (?cwd=...&cols=...&rows=...[&server=<id>]) и control-протокол (resize / switch_cwd) идентичны. Все три внутри переходят в generic handle_tui_socket<F> (ws.rs) с разной spawn-функцией.

## Прочее

- After Phase 3 forge-gda удалены REST git API routes (status/log/stage/unstage/commit), DTO (LogQuery, PathsReq, CommitReq) — git-функционал переехал на WS endpoint /ws/lazygit (lazygit TUI в браузере).
- Static-asset endpoint (rust-embed): GET /, /app.js, /style.css, /index.html — frontend SPA.
- tracing init: env-controlled (RUST_LOG).
