# openTodoEditModal

Открывает модалку редактирования TODO. Доступные действия: Save (PATCH /api/todos/:id), Delete (DELETE /api/todos/:id), Promote (promoteTodo с tmux session).

# Phase 3 (Tasks UI integration): confirm перед DELETE управляется флагом

Delete-кнопка теперь оборачивает window.confirm() проверкой state.userSettings.todo_confirm_delete:

  const confirmDelete = !(state.userSettings && state.userSettings.todo_confirm_delete === false);
  if (confirmDelete && !window.confirm('Удалить TODO?')) return;

Поведение по веткам:
- state.userSettings == null (preload fail / not yet loaded) → confirmDelete === true → confirm показывается всегда (legacy).
- state.userSettings.todo_confirm_delete === undefined → confirmDelete === true → confirm (forward-compat / partial JSON).
- state.userSettings.todo_confirm_delete === true (default) → confirm.
- state.userSettings.todo_confirm_delete === false → confirm пропускается, сразу DELETE.

# ИНВАРИАНТ

Дефолт todo_confirm_delete = true (как в UserSettings::default и в todo-tab.js initial). При нулевой конфигурации (~/.forge/user_settings.json отсутствует) поведение совпадает с legacy — confirm всегда показывается.

# Файл

tmux-web/static/js/tasks/modals.js.

# Зависимости

- state, apiFetch, dtoOrigin, buildModalOverlay, promoteTodo.
