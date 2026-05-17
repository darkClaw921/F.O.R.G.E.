# plugins/echo/src/claude/mod.rs::RunRequest

Запрос на запуск Claude. Поля: prompt (полный, уже собранный prompt_builder'ом), model (Option<String> для --model), system (Option<String> для --append-system-prompt), run_id (RunId=String для cancel). RunRequest::new(prompt) — конструктор для тестов с авто-UUID.
