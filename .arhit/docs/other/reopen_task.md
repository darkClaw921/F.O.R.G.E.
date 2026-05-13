# reopen_task

Axum-хендлер POST /api/tasks/:id/reopen. Параметров тела нет. Зовёт br reopen --json <id> через tasks::run_br. br reopen возвращает {reopened: [...]} — отдаём 200 + Json(value). На ошибку — 400. Используется UI-кнопкой Reopen в modal-edit для закрытых задач. Файл: tmux-web/src/main.rs.
