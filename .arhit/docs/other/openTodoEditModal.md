# openTodoEditModal

Phase 4 — модалка редактирования TODO-карточки (tmux-web/static/app.js). Поля: title, description, session-input (Promote → tmux session). default session = state.currentSession || первая сессия проекта. Кнопки: Cancel (закрыть), Delete (DELETE /api/todos/:id с confirm), Promote (вызывает promoteTodo с введённым session), Save (PATCH /api/todos/:id с только изменёнными полями). Без поля status — у TODO-карточек его нет. WS upsert/removed синхронизирует state.todosData автоматически после успеха.
