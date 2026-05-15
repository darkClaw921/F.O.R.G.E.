# telescope_attach

Axum ws-handler для маршрута GET /ws/telescope. Зеркало lazygit_attach для TUI-вкладки tv (television, fuzzy finder).

Сигнатура: pub async fn telescope_attach(ws: WebSocketUpgrade, State<AppState>, Query<HashMap<String,String>>) -> Response

Поведение:
- При ?server=<id> в remote-mode → проксирует через remote_proxy::proxy_websocket с upstream_path='/ws/telescope'.
- При ?server=<id> в non-remote-mode → Close{1008, 'remote mode disabled'}.
- Локально: parse_lazygit_query → ws.on_upgrade → handle_tui_socket(socket, q, spawn_television, 'telescope').

Источник: tmux-web/src/ws.rs. Маршрут в tmux-web/src/main.rs.
