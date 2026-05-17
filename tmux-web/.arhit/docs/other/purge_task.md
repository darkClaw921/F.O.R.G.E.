# purge_task

Backend handler tmux-web/src/main.rs. POST /api/tasks/:id/purge — физически удаляет issue через 'br delete --hard --force --json --reason clean-column <id>'. Маршрут зарегистрирован в .route("/api/tasks/:id/purge", post(purge_task)). Поддерживает remote-proxy через try_proxy_to_remote. cwd берётся из активного проекта. Успех → 204 No Content; ошибка br → 400. Использован bulk-clean кнопкой frontend (forge-caz3) для очистки колонки Closed. Флаг --force даёт удалить даже если есть зависимые issues (избегаем сбоев при массовой очистке).
