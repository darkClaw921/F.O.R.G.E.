# tmux-web/src/main.rs::get_todos

GET /api/todos?path=<cwd> — REST handler возврата TODO-карточек для root.

### Контракт
- Query: path (обязательный, cwd сессии). Пустая строка или отсутствие → 400 Bad Request.
- Response: 200 + JSON-массив Todo для root, либо [] если в root нет TODO.

### Алгоритм
1. Если query содержит ?server=<id> и remote_mode=true — proxy на upstream через try_proxy_to_remote.
2. Извлечь path из query → 400 если пусто.
3. paths::resolve_root(path) — спуск по дереву: .beads/ → .git/ → сам cwd.
4. TodoStore::list(root) — вернуть карточки.

### Phase 2 изменения
Раньше handler брал ?project_id= или fallback на state.projects.active().id. Теперь только cwd-based маршрутизация без project context.
