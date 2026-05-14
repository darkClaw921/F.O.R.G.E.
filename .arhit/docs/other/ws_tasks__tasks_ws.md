# ws_tasks::tasks_ws

WebSocket-handler /ws/tasks: realtime task-стрим из per-connection beads watcher или прокси на remote.

## Сигнатура
```rust
pub async fn tasks_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response
```

## Query
- project_id (optional) — id проекта или '__path__:<abs>'. Default — активный проект.
- **server** (optional, Phase 4) — id remote-devforge для прокси.

## Логика (Phase 4)
1. Если ?server=<id> + remote_mode=true → proxy_websocket на upstream /ws/tasks (с query без server).
2. Если ?server=<id> + remote_mode=false → Close{1008, 'remote mode disabled'}.
3. Иначе — resolve_project_path → handle_socket (per-conn notify watcher на .beads/issues.jsonl).

## Wire-протокол (локальный)
- Server→Client (Text): {kind:snapshot,data:...} | {kind:upsert,issue:...} | {kind:removed,id:...} | {kind:reload}.
- Client→Server: только Pong + Close.

## Helpers
- extract_server_id, rebuild_query_without_server, urlencode_minimal, close_with_policy_violation — приватные локальные дубликаты helper'ов из main.rs (для WS-handler'ов).
