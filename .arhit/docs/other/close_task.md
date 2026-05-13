# close_task

Axum-хендлер DELETE /api/tasks/:id?reason=... Принимает CloseTaskQuery {reason?}. Зовёт br close --json <id> [-r reason] через tasks::run_br. На успех — 204 NoContent (тело не возвращаем — клиент уже знает id). На ошибку — 400 + stderr. Используется UI-кнопкой Close в modal-edit. Файл: tmux-web/src/main.rs.
