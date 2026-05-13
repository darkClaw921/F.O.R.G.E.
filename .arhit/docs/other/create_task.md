# create_task

Axum-хендлер POST /api/tasks. Принимает CreateTaskReq {title, description?, type?, priority?, labels?, parent?}. Trim/validate title (400 если пусто), priority диапазон 0..=4 (400 при превышении). Берёт cwd из active project (RwLock read). Динамически собирает Vec<String> args для br create --json: --title (обязателен), -t/-p/-d/-l/--parent (если заданы). Конвертит в &[&str] и зовёт tasks::run_br. На успех — 201 Created + Json(serde_json::Value) с распарсенным issue из stdout br. На ошибку run_br — 400 BadRequest + текст ошибки. Файл: tmux-web/src/main.rs.
