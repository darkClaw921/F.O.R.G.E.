# tmux-web/src/main.rs::promote_todo

POST /api/todos/:id/promote — конвертирует TODO в beads-задачу (br create) и кидает уведомление в notifier.

### Body PromoteTodoReq
{ session: String } — обязательно: tmux-сессия, куда летит NotifyJob. Phase 3 заменит на global notifier config + опциональный override.

### Алгоритм
1. proxy-check (?server=<id>).
2. Парсить body (пустое body → session=None). 400 если session пуст.
3. TodoStore::get(id) → 404 если нет.
4. tasks::run_br(['create', '--json', '--title', ..., '-d', ..., '-t', ..., '-p', ...], cwd=todo.root_path). Если фейл — 400.
5. Извлечь task_id из ответа br (json.id ИЛИ json.created[0].id).
6. TodoStore::delete(id) + broadcast TodoEvent::Removed(root_path, id).
7. Сформировать text через format_notify_template(DEFAULT_NOTIFY_TEMPLATE, ...). Если todo.plan_mode — добавить suffix (user_settings.todo_plan_mode_suffix или PLAN_MODE_SUFFIX).
8. NotifyJob {project_id: todo.root_path, task_id, target_session, text, mode: Immediate} → notifier.enqueue(job).
9. 200 + { promoted: true, task_id }.

### Phase 2 изменения
- Убран project lookup (state.projects.read()).
- session берётся только из body.session (раньше был fallback на project.notify_session/tmux_prefix).
- run_br с cwd = todo.root_path (раньше project.path).
- Template = константа DEFAULT_NOTIFY_TEMPLATE (раньше project.notify_template).
- NotifyMode = Immediate (раньше Wait/Delayed по project флагам).
- NotifyJob.project_id заполняется todo.root_path как hack — Phase 3 переименует поле.
