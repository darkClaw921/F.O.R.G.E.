# promote_todo

POST /api/todos/:id/promote — конвертирует TODO-карточку в bd-задачу и (если есть целевая сессия) отправляет её текст в активную tmux-сессию через notifier.

Алгоритм:
1. Найти TODO (404 если нет).
2. Создать bd-issue через 'br create' в todo.root_path, извлечь id из JSON.
3. Удалить TODO + broadcast Removed.
4. Резолвить target_session: body.session > cfg.session (NotifierConfig).
5. Если target_session не определён — skip notify (200, notify_scheduled=false). bd-задача всё равно создана.
6. Сформировать текст: если cfg.template ПУСТ — используется DEFAULT_PROMOTE_TEMPLATE ('{title}'), а непустое описание дописывается отдельной строкой (заголовок\nописание). Если template задан — рендерится через format_notify_template. При todo.plan_mode дописывается plan-mode suffix.
7. mode: wait_previous > delay_minutes>0 > Immediate. enqueue в notifier.

ВАЖНО (фикс бага 'текст не уходит в сессию'): пустой template НЕ блокирует отправку — раньше ранний return срабатывал при template_trimmed.is_empty(), теперь только при target_session.is_none(). Это сохраняет zero-config: перенос TODO→open сразу шлёт текст задачи в активную сессию.
