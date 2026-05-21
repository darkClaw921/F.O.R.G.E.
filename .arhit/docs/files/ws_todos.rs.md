# ws_todos.rs

WebSocket-handler /ws/todos для realtime TODO-стрима. После remove-projects-concept (Phase 5.5) query-параметр сменился с project_id на path: TodoWsQuery { path: Option<String> }. Если path не задан — fallback на active_path из state.active_path_tx. Передаваемый cwd резолвится через paths::resolve_root(cwd) в root_path, по которому фильтруются TodoEvent broadcast'ы.

## TodoEvent
enum TodoEvent с serde tag='kind':
- Upsert(Todo) — добавление/обновление карточки.
- Removed { root_path, id } — удаление.
- Reload { root_path } — полная перезагрузка списка root_path (например после миграции или batch-операции).

## Жизненный цикл соединения
1. Snapshot: при connect отправляется список todos.list(root_path) для резолвнутого пути.
2. Loop: select между broadcast::Receiver<TodoEvent> (фильтрация по root_path) и heartbeat ping каждые 30с.
3. Lag recovery: при RecvError::Lagged отправляется {kind:reload, root_path} — клиент перезапрашивает snapshot.

## Helpers
- upsert(todo) — фабрика TodoEvent::Upsert для использования из REST-handler'ов.
- removed(root_path, id) — фабрика TodoEvent::Removed.
- reload(root_path) — фабрика TodoEvent::Reload.

## Зависимости
- AppState.todos (TodoStore) — для snapshot'а через list(root_path) или list_by_cwd(cwd).
- AppState.todos_tx (broadcast::Sender<TodoEvent>) — источник событий.
- paths::resolve_root — резолв cwd в root_path.
- todos::Todo — DTO.
