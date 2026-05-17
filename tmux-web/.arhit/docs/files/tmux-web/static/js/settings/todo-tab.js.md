# tmux-web/static/js/settings/todo-tab.js

Фабрика DOM-формы для вкладки 'TODO behavior' в Settings modal. Реализует UI редактора пользовательских настроек (6 контролов, мэппинг 1:1 на UserSettings из tmux-web/src/user_settings.rs).

# Экспорт

buildTodoBehaviorForm(settings, onSaved) -> HTMLFieldSetElement
- settings: текущий объект state.userSettings (или null — используются локальные дефолты).
- onSaved: callback(updatedSettings) — вызывается после успешного Save.
- Возвращает <fieldset>, который вставляется в контейнер вкладки в settings/modal.js.

# Контролы (6 шт.)

Каждый контрол маппится на одно поле UserSettings:

1. todo_default_plan_mode — <input type='checkbox' class='todo-default-plan-mode'>. Initial: !!s.todo_default_plan_mode (undefined → false).
2. todo_default_priority — <select class='todo-default-priority'> с опциями 0..4 (critical/high/medium/low/backlog). Initial: Number.isFinite(s.todo_default_priority) ? Number(s.todo_default_priority) : 2.
3. todo_default_issue_type — <select class='todo-default-issue-type'> с опциями task/bug/feature/epic/chore/docs/question. Initial: s.todo_default_issue_type || 'task'.
4. todo_plan_mode_suffix — <textarea rows=3 class='todo-plan-mode-suffix'>. placeholder = 'Создай план для этой задачи' (DEFAULT_PLAN_MODE_SUFFIX). Подсказка под полем: 'Если пусто — используется значение по умолчанию: «...»'. Пустая строка сохраняется КАК ЕСТЬ — backend (promote_todo) сам решит делать fallback к PLAN_MODE_SUFFIX.
5. todo_confirm_delete — <input type='checkbox' class='todo-confirm-delete'>. Initial: s.todo_confirm_delete === undefined ? true : !!. Дефолт true — это критично для legacy-инварианта (раньше confirm всегда был).
6. todo_confirm_promote_on_drag — <input type='checkbox' class='todo-confirm-promote-on-drag'>. Initial: undefined → false. Дефолт false — drop без confirm (как было).

# CSS reuse

Использует те же CSS-классы, что и notifications-tab.js (см. styles.css): .notify-fieldset, .notify-field, .notify-hint, .notify-error, .notify-actions, .modal-check, .notify-check. Это значит, что НИКАКИХ изменений CSS не требуется — стиль автоматически согласован с другими вкладками настроек.

# Save flow

1. Click Save: hide error+ok, disable button.
2. Собрать payload — 6 полей, с safety-clamping priority в 0..4 (если parseInt → NaN, fallback к 2).
3. await updateUserSettings(payload) (см. user-settings-api.js, optimistic + rollback внутри).
4. На успехе: показать '.todo-save-ok' на 2 секунды, вызвать onSaved(updated).
5. На ошибке: показать '.notify-error' с e.message (или 'Не удалось сохранить настройки.').
6. finally: re-enable button.

# Локальные дефолты

DEFAULT_PLAN_MODE_SUFFIX = 'Создай план для этой задачи' — placeholder и hint. ВНИМАНИЕ: это лейбл UI, не значение по умолчанию для backend. На backend пустая строка → константа PLAN_MODE_SUFFIX из main.rs (не обязательно та же текстовая строка).

PRIORITY_OPTIONS, ISSUE_TYPE_OPTIONS — константы массивов опций для select.

# Связи

- settings/modal.js: вызывает buildTodoBehaviorForm(state.userSettings, onSaved), вставляет результат в DOM вкладки.
- settings/user-settings-api.js: импортирует updateUserSettings.
- styles.css: переиспользует .notify-* классы.

Файл: tmux-web/static/js/settings/todo-tab.js (~213 строк, ~10KB).
