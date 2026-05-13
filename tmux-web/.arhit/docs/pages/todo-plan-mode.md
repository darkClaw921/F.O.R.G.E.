Фича plan_mode для TODO-карточек (forge-5bkg, 2026-05-11).

## Суть
Каждая TODO-карточка получила булевый флаг plan_mode (по умолчанию false). При promote TODO → bd-task, если plan_mode=true, к тексту notify-сообщения, отправляемому через tmux::send_keys в активную сессию, добавляется новая строка с константой PLAN_MODE_SUFFIX = 'Создай план для этой задачи'.

## Backend (tmux-web/src/...)
- todos.rs::Todo — поле plan_mode: bool #[serde(default)]. Совместимость со старыми todos.json через default=false.
- todos.rs::TodoStore::create(project_id, title, description, plan_mode) — новый параметр.
- todos.rs::TodoStore::update(id, title, description, plan_mode: Option<bool>) — добавлен 4-й параметр. None=не трогать, Some(bool)=перезаписать.
- main.rs::CreateTodoReq — поле plan_mode: bool #[serde(default)] (POST /api/todos).
- main.rs::PatchTodoReq — поле plan_mode: Option<bool> #[serde(default)] (PATCH /api/todos/:id).
- main.rs::PLAN_MODE_SUFFIX — константа 'Создай план для этой задачи'.
- main.rs::promote_todo — после format_notify_template() если todo.plan_mode → text += '\n' + PLAN_MODE_SUFFIX (новая строка добавляется только если text непуст и не оканчивается на \n).

## Frontend (tmux-web/static/app.js + style.css)
- openCreateModal({status:'todo'}) — рендерит checkbox #tm-plan-mode 'Включить план мод' под формой; payload содержит plan_mode: bool в POST /api/todos.
- openTodoEditModal — рендерит checkbox #td-plan-mode (initial = todo.plan_mode); patch включает plan_mode только если значение изменилось vs текущего.
- renderTodoCard — рисует .plan-mode-badge '◆ plan' в meta-row если todo.plan_mode=true.
- style.css — стили .modal-card .checkbox-row (flex с input checkbox + .hint), .plan-mode-badge (фиолетовый pill #6a4bb0).

## Тесты
todos::tests::create_get_update_delete_roundtrip — проверяет: t.plan_mode=false после create(.., false), updated.plan_mode=true после update(.., Some(true)). Фикстуры в ws_todos::tests дополнены plan_mode: false. cargo test: 69 passed.

## Обратная совместимость
Старые todos.json без plan_mode-поля грузятся OK (serde default=false). Старые UI-клиенты без чекбокса посылают POST/PATCH без plan_mode → серверу всё равно (Option<bool>::None = не трогать).