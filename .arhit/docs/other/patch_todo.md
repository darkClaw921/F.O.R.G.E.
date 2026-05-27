# patch_todo

PATCH /api/todos/:id (main.rs). Принимает PatchTodoReq{title,description(Option<Option>),plan_mode,auto_promote,path}. Фаза 2 auto_promote: PatchTodoReq получил поле #[serde(default)] auto_promote: Option<bool> (None=не трогать, Some(b)=записать); patch_todo прокидывает req.auto_promote пятым аргументом в state.todos.update(&id,title,description,plan_mode,auto_promote). После апдейта (и опц. move в новый корень через paths::resolve_root при наличии path) шлёт WS Upsert клиентам. 404 если id не найден.
