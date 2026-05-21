# tmux-web/src/main.rs::patch_todo

PATCH /api/todos/:id — REST handler обновления TODO.

### Body PatchTodoReq
- title?: String — переименовать (пустое после trim → 400).
- description?: Some(String)|null — Some → записать, null → стереть; отсутствие поля → не трогать (custom deserializer deserialize_optional_optional_string).
- plan_mode?: bool — обновить флаг.
- path?: String — move TODO в новый корень через paths::resolve_root(path).

### Алгоритм
1. proxy-check (?server=<id>).
2. Парсить body, валидировать title.
3. TodoStore::update(&id, title, description, plan_mode) → 404 если id нет.
4. Если body.path задан и (после resolve_root) отличается от текущего root_path — TodoStore::move_to_root(id, new_root). Это move между bucket'ами; шлёт TodoEvent::Removed для старого root_path (старый подписчик увидит удаление).
5. Broadcast TodoEvent::Upsert (для нового root_path).
6. 200 + Json<Todo>.

### Phase 2 изменения
Добавлено поле path в PatchTodoReq для move TODO между корнями cwd. Раньше TODO не могла менять project. Также: handler больше не использует state.projects.
