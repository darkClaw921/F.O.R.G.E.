# state.userSettings

Поле state.userSettings в tmux-web/static/js/core/state.js (Phase 2 feature).

Значение: кэш пользовательских настроек TODO behavior, загружается через GET /api/user-settings.
- null — до первого успешного fetchUserSettings (или после ошибки fetch).
- object — { todo_default_plan_mode, todo_default_priority, todo_default_issue_type, todo_plan_mode_suffix, todo_confirm_delete, todo_confirm_promote_on_drag }.

Контракт для callers:
- Tasks UI (Phase 3) обращается к state.userSettings при создании TODO, при удалении, при drag promote. Если null — используются legacy-дефолты (поведение совпадает с тем, что было до фичи).
- settings/modal.js → вкладка 'TODO behavior' (lazy reload при первом клике если userSettings === null).

Запись: fetchUserSettings/updateUserSettings из settings/user-settings-api.js.
