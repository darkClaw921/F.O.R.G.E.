# renderColumn

Рендерит колонку kanban-доски (header + body + drag/drop handlers).

# Drop-handler логика

- isTodoPayload (raw начинается с 'todo:'): допустим только drop в колонку 'open' → promoteTodo(todoId).
- обычная задача: targetStatus !== 'todo' → updateTask(id, {status: targetStatus}).

# Phase 3 (Tasks UI integration): drop TODO→Open с опциональным confirm

Добавлена проверка флага перед promoteTodo при drag TODO→Open:

  const needConfirm = !!(state.userSettings && state.userSettings.todo_confirm_promote_on_drag === true);
  if (needConfirm && !window.confirm('Promote TODO в bd-задачу?')) return;
  promoteTodo(todoId);

Поведение по веткам:
- state.userSettings == null или todo_confirm_promote_on_drag !== true → needConfirm = false → promote сразу без confirm (legacy default).
- todo_confirm_promote_on_drag === true → confirm показывается; cancel блокирует promote, ok пропускает.

# ИНВАРИАНТ

Дефолт todo_confirm_promote_on_drag = false (как в UserSettings::default и в todo-tab.js initial). При нулевой конфигурации drag TODO→Open работает мгновенно — как до фичи.

# Прочее

- col-add кнопка (статус !== 'closed') вызывает openCreateModal({status}).
- clean-кнопка массово закрывает/удаляет задачи колонки.

# Файл

tmux-web/static/js/tasks/render.js.

# Зависимости

- state, openCreateModal, openEditModal, openTodoEditModal, updateTask, promoteTodo, cleanColumn.
