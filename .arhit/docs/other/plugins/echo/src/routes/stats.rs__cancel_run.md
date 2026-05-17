# plugins/echo/src/routes/stats.rs::cancel_run

POST /api/echo/run/:id/cancel — отменяет активный run через ClaudeRunner::cancel. 200 если run найден и aborted, 404 если такого run нет (уже завершён или невалидный id). Резервный путь cancel — основной канал через WebSocket ClientMsg::Cancel.
