# tmux-web/src/ws_todos.rs::TodoEvent

Событие TODO для broadcast-канала tokio. Сериализуется через serde с tag='kind', rename_all='snake_case'.

### Варианты
- Upsert { todo: Todo } — TODO создана/обновлена. Поле root_path извлекается из todo.root_path.
- Removed { root_path: String, id: String } — TODO удалена.
- Reload { root_path: String } — сигнал клиентам ресинхронизироваться через GET /api/todos?path=.

### Метод root_path() -> &str
Возвращает root_path события — используется сервером для фильтрации broadcast-стрима по root_path подписчика на /ws/todos.

### JSON-формат (wire)
- Upsert: {"kind":"upsert","todo":{...}}
- Removed: {"kind":"removed","root_path":"/abs/...","id":"..."}
- Reload: {"kind":"reload","root_path":"/abs/..."}

### История
Phase 1: поле в Todo переименовано project_id → root_path.
Phase 2: поля в Removed/Reload переименованы project_id → root_path (полное удаление концепции проектов). Метод project_id() -> root_path(). Helper-функции removed(root_path, id) и reload(root_path).

### Helper'ы
- ws_todos::upsert(todo) -> TodoEvent::Upsert
- ws_todos::removed(root_path, id) -> TodoEvent::Removed
- ws_todos::reload(root_path) -> TodoEvent::Reload (allow(dead_code))
