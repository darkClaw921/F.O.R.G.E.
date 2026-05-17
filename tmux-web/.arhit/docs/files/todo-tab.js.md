# todo-tab.js

ES-module: вкладка 'TODO behavior' в Settings modal (Phase 2 feature). Путь: tmux-web/static/js/settings/todo-tab.js.

Экспортирует buildTodoBehaviorForm(settings, onSaved) — фабрика DOM-узла (fieldset.notify-fieldset). Возвращает свежий узел при каждом вызове. settings может быть null/{} — тогда применяются дефолты, совпадающие с backend-ом (src/user_settings.rs).

Контролы (6 штук, 1:1 с backend struct):
1. checkbox .todo-default-plan-mode — todo_default_plan_mode (default false).
2. select .todo-default-priority — todo_default_priority 0..4 (default 2, labels 'critical/high/medium/low/backlog').
3. select .todo-default-issue-type — task/bug/feature/epic/chore/docs/question (default 'task').
4. textarea .todo-plan-mode-suffix (3 rows) — todo_plan_mode_suffix; placeholder='Создай план для этой задачи'; hint про дефолт при пустой строке.
5. checkbox .todo-confirm-delete — todo_confirm_delete (default true; undefined → true).
6. checkbox .todo-confirm-promote-on-drag — todo_confirm_promote_on_drag (default false).

Save button:
- Собирает полный payload (все 6 полей, с clamp priority 0..4 и fallback на 2 при NaN).
- Вызывает updateUserSettings(payload) из user-settings-api.js — клиент сам делает optimistic update + rollback.
- На успехе: показывает inline-success ('Сохранено') на 2 сек, вызывает onSaved?.(updated).
- На ошибке: показывает inline-error с e.message.
- saveBtn.disabled во время запроса, восстанавливается в finally.

Стилизация: переиспользует классы .notify-fieldset/.notify-field/.notify-hint/.notify-error/.notify-actions/.modal-check из notifications-tab.js — никаких новых CSS не требуется.

Зависимости: updateUserSettings из ./user-settings-api.js.
Использование: settings/modal.js → renderTodoPanel() при первом клике на вкладку TODO behavior.
