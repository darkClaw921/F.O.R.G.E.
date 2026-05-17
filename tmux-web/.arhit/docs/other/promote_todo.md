# promote_todo

REST-handler POST /api/todos/:id/promote в tmux-web/src/main.rs (~line 2411).

Конвертирует TODO в полноценный bd-task с уведомлением в tmux:
1. Читает TODO из state.todos.
2. Создаёт задачу через 'br create' (форк процесса).
3. Удаляет TODO + broadcast TodoEvent::Removed.
4. Формирует notify-текст через format_notify_template (с подстановкой {id}, {title}, {description}, {priority}, {type}).
5. Если todo.plan_mode == true — добавляет suffix:
   - Берёт state.user_settings.get().todo_plan_mode_suffix.
   - Если suffix.trim().is_empty() → используется константа PLAN_MODE_SUFFIX = 'Создай план для этой задачи'.
   - Иначе — кастомный suffix из user-settings.
   - Добавляет '\n' перед suffix если text непустой и не оканчивается на '\n'.
6. Формирует NotifyJob (Immediate / Delayed / WaitPrevious — по project_snap.notify_delay_minutes / notify_wait_previous).
7. Enqueue в state.notify (NotifyHandle).

# Связь с user_settings (Phase 1.4)

state.user_settings: UserSettingsStore (из AppState) читается на каждом вызове через .get() — это клон под read-lock, дёшево. Не кэшируется в локальный snapshot, потому что в моменте promote пользователь мог только что обновить suffix через PATCH /api/user-settings.

КРИТИЧЕСКИЙ ИНВАРИАНТ: при пустом todo_plan_mode_suffix в user-settings (default state) поведение promote_todo побитово идентично состоянию до фичи user-settings — используется константа PLAN_MODE_SUFFIX. Это гарантирует, что пользователи без созданного ~/.forge/user_settings.json не заметят разницы.

# Связи

- user_settings.rs: UserSettings.todo_plan_mode_suffix.
- main.rs: константа PLAN_MODE_SUFFIX (legacy fallback).
- AppState.user_settings: UserSettingsStore.
