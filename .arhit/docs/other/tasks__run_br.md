# tasks::run_br

Универсальный async хелпер для запуска CLI `br` через tokio::process::Command. Принимает slice аргументов и cwd, парсит stdout как serde_json::Value. На non-zero exit возвращает anyhow::Error со stderr; на пустой stdout — Value::Null; на непарсимый JSON — Err с preview-сниппетом первых 200 символов. Используется write-хендлерами POST /api/tasks (create), PATCH /api/tasks/:id (update), DELETE /api/tasks/:id (close), POST /api/tasks/:id/reopen — каждый формирует свой массив args с уже добавленным --json. Файл: tmux-web/src/tasks.rs.
