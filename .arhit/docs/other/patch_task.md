# patch_task

Axum-хендлер PATCH /api/tasks/:id. Принимает PatchTaskReq {status?, title?, priority?, description?, labels?}. Все поля опциональны, но если ни одно не задано — 400. priority валидируется 0..=4. labels пробрасывается в --set-labels (replace-семантика, не add). description='' допустимо (стирает). Зовёт br update --json <id> ...args через tasks::run_br. br update --json возвращает массив обновлённых issues — отдаём как есть в Json. На ошибку — 400. Файл: tmux-web/src/main.rs.
