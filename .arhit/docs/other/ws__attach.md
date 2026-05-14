# ws::attach

WebSocket-handler /ws/attach: tmux attach через PTY либо прокси на remote devforge.

## Сигнатура
```rust
pub async fn attach(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response
```

## Query
- session (required) — имя tmux-сессии.
- cols/rows (optional, default 80×24) — стартовый размер PTY.
- **server** (optional, Phase 4) — id remote-devforge для прокси.

## Логика (Phase 4)
1. Извлекает ?server=<id> через extract_server_id.
2. Если server есть + state.remote_mode=false → upgrade и Close{1008, 'remote mode disabled'}.
3. Если server есть + state.remote_mode=true → upgrade и proxy_websocket(store, id, '/ws/attach', query_без_server, socket).
4. Иначе — парсит AttachQuery через parse_attach_query и идёт в локальный handle_socket (spawn tmux-PTY).

## Wire-протокол (локальный путь)
- Binary frames в обе стороны — сырые байты PTY.
- Text frames от клиента — JSON control: {type:resize,cols,rows} или {type:switch,session}.
- Close frame — teardown.

## Поведение при ошибках
- Невалидный query (нет session или плохой cols/rows) — upgrade и Close{1008, 'invalid query'}.
- spawn_tmux_attach fail — Text frame с ошибкой + Close.

## См. также
- ws::handle_socket — основной обработчик локального PTY.
- remote_proxy::proxy_websocket — WS-прокси для ?server веток.
