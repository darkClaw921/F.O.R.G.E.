# promote_todo

POST /api/todos/:id/promote — конвертирует TODO в bd-задачу + опциональный notify.

## Phase 3 алгоритм

1. todos.get(id) — 404 если нет TODO.
2. br create --json в todo.root_path → bd-issue с task_id.
3. todos.delete + broadcast TodoEvent::Removed (root_path scoped).
4. cfg = state.notifier_config.get().
   - target_session = body.session OR cfg.session. Если оба пусты → skip notify.
   - template = cfg.template. Если пусто → skip notify.
5. Если skip: 200 OK { promoted: true, task_id, notify_scheduled: false }.
6. Иначе: text = format_notify_template(cfg.template, ...) + plan_mode_suffix.
7. mode по приоритету: cfg.wait_previous → WaitPrevious{None}; cfg.delay_minutes>0 → Delayed; иначе Immediate.
8. notifier.enqueue(NotifyJob{ root_path: todo.root_path, ... }).
9. 200 OK { promoted: true, task_id, notify_scheduled: true }.

## Отличия от Phase 2
- Нет lookup'а Project — ни в , ни в .
- body.session больше не обязателен (если задана cfg.session — она используется).
- Notify может не запланироваться (если template/session пусты) — bd-задача всё равно создаётся.
- NotifyJob.root_path = todo.root_path (cwd-only ключ).

## Plan-mode suffix
Если todo.plan_mode=true, к тексту прикрепляется через  суффикс. Источник: user_settings.todo_plan_mode_suffix; если пуст — константа PLAN_MODE_SUFFIX.
