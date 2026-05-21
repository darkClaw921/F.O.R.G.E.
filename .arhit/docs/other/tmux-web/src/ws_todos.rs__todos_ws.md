# tmux-web/src/ws_todos.rs::todos_ws

GET /ws/todos?path=<cwd> — WebSocket upgrade handler.

### Поведение
- ?server=<id> + remote_mode=true → proxy_websocket на upstream (без server в query).
- ?server=<id> + remote_mode=false → Close{1008}.
- Иначе: subscribed_root = Some(paths::resolve_root(path).to_string_lossy()) если path задан и непуст; иначе None.

### Subscribed root semantics
- Some(root): snapshot = TodoStore::list(root); фильтрация TodoEvent по event.root_path() == root.
- None: snapshot = []; форвардит ВСЕ события (admin/debug mode).

### Phase 2 изменения
- ?project_id= заменён на ?path=.
- При path сервер делает paths::resolve_root() и фильтрует по полученному root_path.
- Без path: вместо fallback на active project — admin/debug режим (все события).
