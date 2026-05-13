# promote_todo

Phase 3 — handler POST /api/todos/:id/promote в tmux-web/src/main.rs. Конвертирует TODO-карточку в bd-задачу через 'br create --json' + ставит NotifyJob в notifier-очередь.

Алгоритм:
1. todos.get(id) → 404 если нет.
2. projects.find_any(todo.project_id) → 500 если пропал.
3. resolve_notify_session(req.session, project) → 400 если None.
4. br_args: ['create', '--json', '--title', todo.title, (-d desc)?, (-t issue_type)?, '-p', priority]; tasks::run_br(args, project.path) → JSON → task_id (поля 'id' или 'created[0].id').
5. todos.delete(id) + broadcast TodoEvent::Removed.
6. text = format_notify_template(template || DEFAULT_NOTIFY_TEMPLATE, task_id, todo.title, desc, priority, issue_type).
   - forge-5bkg: если todo.plan_mode == true → text += '\n' + PLAN_MODE_SUFFIX ('Создай план для этой задачи'). Префикс '\n' добавляется только если text непуст и не оканчивается на \n.
7. mode = WaitPrevious (если notify_wait_previous) | Delayed (если notify_delay_minutes>0) | Immediate.
8. notifier.enqueue(notifier::new_job(project_id, task_id, target_session, text, mode)). Ошибка enqueue → 200 + notify_warning (задача уже создана, не катастрофа).
9. Возврат 200 {promoted: true, task_id}.

Используется фронтенд promoteTodo() (drag-drop TODO→open, кнопка ▲, openTodoEditModal Promote).
