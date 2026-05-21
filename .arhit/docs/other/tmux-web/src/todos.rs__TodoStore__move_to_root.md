# tmux-web/src/todos.rs::TodoStore::move_to_root

Перемещает TODO с заданным id в другой root_path (bucket в HashMap<root_path, Vec<Todo>>).

### Параметры
- id: UUID карточки.
- new_root: целевой абсолютный путь (например, результат paths::resolve_root(cwd)).

### Поведение
1. Ищет id по всем bucket'ам Inner.by_path.
2. Если нашёл — удаляет из старого bucket; если bucket опустел — удаляет ключ из HashMap.
3. Обновляет todo.root_path = new_root и todo.updated_at = текущий RFC3339.
4. Вставляет в новый bucket (создаёт если не было).
5. Делает atomic save todos.json.

### Возвращаемое значение
Ok(Some(Todo)) — обновлённая копия после перемещения.
Ok(None) — id не найден ни в одном bucket.
Err — failure атомарного save.

### Используется
PATCH /api/todos/:id в tmux-web/src/main.rs — когда body содержит непустой path, отличный от текущего root TODO. Это move TODO между cwd-корнями.

### Тесты
- move_to_root_relocates_between_buckets
- move_to_root_returns_none_for_unknown_id
- move_to_root_persists_across_reload
