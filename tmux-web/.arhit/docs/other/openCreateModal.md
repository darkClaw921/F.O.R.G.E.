# openCreateModal

Открывает модалку создания задачи/TODO. preset.status определяет режим:
- status='todo' → форма TODO: plan_mode checkbox + project_id из state.activeProjectId, POST /api/todos.
- status иначе → обычная задача через createTask().

# Phase 3 (Tasks UI integration): defaults из state.userSettings

Для TODO-режима (status='todo') initial values 3-х полей берутся из state.userSettings с fallback к legacy-дефолтам при null/undefined:
- plan_mode checkbox.checked ← state.userSettings.todo_default_plan_mode (default false → текущее legacy-поведение).
- priority select.value ← state.userSettings.todo_default_priority (default 2 / medium).
- issue_type select.value ← state.userSettings.todo_default_issue_type (default 'task').

Реализация: вспомогательные локальные функции читают state.userSettings один раз при открытии модалки. Если state.userSettings == null (fetch ещё не отработал или fail) или какое-то поле undefined — fallback к hardcoded legacy-дефолтам.

# ИНВАРИАНТ

При state.userSettings == null или отсутствующих полях — поведение идентично pre-feature legacy (plan_mode=false, P2, type=task). Это критически важно для backward compat — пользователи без user_settings.json не должны заметить изменений.

# Файл

tmux-web/static/js/tasks/modals.js.

# Зависимости

- state (импорт): доступ к state.userSettings и state.activeProjectId.
- apiFetch: POST /api/todos.
- buildModalOverlay: общая инфраструктура модалок.
- createTask, updateTask, fetchTodos.
