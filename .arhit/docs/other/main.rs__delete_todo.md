# main.rs::delete_todo

Phase 3 — DELETE /api/todos/:id: 204 при успехе, 404 если не найден. Перед удалением читает todo.project_id (через todos.get), после успеха → broadcast Removed{project_id, id}.
