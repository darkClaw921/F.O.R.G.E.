# lazydocker_attach

Axum ws-handler для маршрута GET /ws/lazydocker. Зеркало lazygit_attach для отдельной TUI-вкладки lazydocker (Docker manager).

Сигнатура: pub async fn lazydocker_attach(ws: WebSocketUpgrade, State<AppState>, Query<HashMap<String,String>>) -> Response

Поведение:
- При ?server=<id> в remote-mode → проксирует через remote_proxy::proxy_websocket с upstream_path='/ws/lazydocker'.
- При ?server=<id> в non-remote-mode → Close{1008, 'remote mode disabled'}.
- Локально: parse_lazygit_query (общий парсер cwd/cols/rows) → ws.on_upgrade → handle_tui_socket(socket, q, spawn_lazydocker, 'lazydocker').

Источник: tmux-web/src/ws.rs. Маршрут в tmux-web/src/main.rs.
