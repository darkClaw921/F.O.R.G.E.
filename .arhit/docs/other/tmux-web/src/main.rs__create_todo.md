# tmux-web/src/main.rs::create_todo

POST /api/todos — REST handler создания TODO.

### Body
{ path: String, title: String, description?: String, plan_mode?: bool }

### Контракт
- path и title обязательны (после trim непустые). Иначе 400.
- 201 + Json<Todo> при успехе.
- Broadcast TodoEvent::Upsert через state.todos_tx (для /ws/todos подписчиков того же root).

### Алгоритм
1. Если query содержит ?server=<id> — remote proxy.
2. Парсить body → CreateTodoReq.
3. Валидировать title и path.
4. paths::resolve_root(path) → root_key.
5. TodoStore::create(&root_key, ...).

### Phase 2 изменения
Раньше body имел опциональное project_id с fallback на active project. Теперь body.path обязателен и резолвится через paths::resolve_root.
