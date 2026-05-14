# ws::lazygit_attach

WebSocket-handler /ws/lazygit: lazygit в PTY либо прокси на remote.

## Сигнатура
```rust
pub async fn lazygit_attach(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response
```

## Query
- cwd (required) — абсолютный путь к git-репо или подкаталогу.
- cols/rows (optional, default 80×24).
- **server** (optional, Phase 4) — id remote-devforge для прокси.

## Логика (Phase 4)
1. Если ?server=<id> + remote_mode=true → proxy_websocket на upstream /ws/lazygit.
2. Если ?server=<id> + remote_mode=false → Close{1008, 'remote mode disabled'}.
3. Иначе — parse_lazygit_query + handle_lazygit_socket (spawn lazygit в cwd).

## Wire-протокол (локальный)
- Binary in/out — PTY байты.
- Text от клиента (snake_case tag): {type:resize,cols,rows} | {type:switch_cwd,cwd}.
- Error frame: {type:error,message:...}.

## См. также
- ws::handle_lazygit_socket — локальная логика.
- remote_proxy::proxy_websocket.
