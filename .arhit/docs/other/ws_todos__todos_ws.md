# ws_todos::todos_ws

WebSocket-handler /ws/todos: realtime TODO-стрим или прокси на remote.

## Сигнатура
```rust
pub async fn todos_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response
```

## Query
- project_id (optional) — id проекта. Default — активный.
- **server** (optional, Phase 4) — id remote-devforge для прокси.

## Логика (Phase 4)
1. ?server=<id> + remote_mode=true → proxy_websocket('/ws/todos').
2. ?server=<id> + remote_mode=false → Close{1008}.
3. Иначе — handle_socket с фильтрацией broadcast по project_id.

## Wire-протокол (локальный)
- Server→Client (Text): {kind:snapshot,todos:[...]} | {kind:upsert,todo:...} | {kind:removed,project_id,id} | {kind:reload,project_id}.
- На lag broadcast канала → шлём {kind:reload} (клиент делает fetchTodos).
