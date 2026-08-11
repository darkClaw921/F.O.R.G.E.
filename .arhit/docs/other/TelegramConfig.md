# TelegramConfig

Конфигурация плагина Telegram (plugins/telegram/src/config.rs).

Цепочка приоритетов: default() -> load_from_file(~/.config/forge/telegram.toml, путь через FORGE_TELEGRAM_CONFIG_PATH) -> apply_env(FORGE_TELEGRAM_*) -> validate_and_fix(). load() делает всё вместе и НИКОГДА не паникует: нет файла, битый TOML, мусор в env, невалидное значение — всё это warn-лог и откат на дефолт.

ПОЛЯ MTProto: api_id / api_hash (намеренно Option — пользователь вводит их в UI, конфиг обязан быть валидным и без них), session_path, cache_dir, max_upload_bytes, max_cache_bytes, catch_up, update_queue_limit.

ПОЛЯ «АНАЛИЗА ЧАТА» (добавлены на Этапе 2, значения совпадают с VK намеренно — это ОДНА фича на три мессенджера, и разные умолчания означали бы, что в одной вкладке разбор укладывается в бюджет, а в соседней нет):
- prompts_path — файл заметок, разборов и тредов Q&A. Дефолт ~/.forge/telegram-chat-prompts.json, env FORGE_TELEGRAM_PROMPTS_PATH. Помечен serde(skip): TOML им НЕ управляет, потому что это пользовательские данные вкладки, а не настройка машины. Env-override здесь не теоретическая аккуратность: без него любой cargo test, дёрнувший сохранение, затирал бы живые заметки разработчика.
- claude_cli_path — бинарь Claude CLI. В ОТЛИЧИЕ от prompts_path читается из TOML (это настройка машины). Дефолт /Users/igorgerasimov/.local/bin/claude — тот же, что у forge-echo и forge-vk: все три запускают ОДИН CLI, и разные умолчания означали бы, что чат Echo работает, а анализ Telegram нет, без единого сообщения о причине. Существование бинаря при загрузке НЕ проверяется: анализ не условие работы вкладки, и отсутствие CLI не имеет права ронять старт. Отказ выносит роут кодом 409 claude_unavailable и называет ровно этот путь.
- analysis_timeout_secs (дефолт 300; 0 — легальное «без таймаута»). Зависший процесс держал бы слот MAX_PARALLEL_ANALYSIS вечно.
- analysis_max_chars (дефолт 60000, клампится в 5000..=200000). Ниже 5000 транскрипт перестаёт быть перепиской; выше 200000 — это ~50 тысяч токенов на запрос, то есть опечатка, а не намерение.
- analysis_model (дефолт "sonnet"). None ⇒ флаг --model не передаётся и решает сам CLI. ПУСТАЯ СТРОКА в env ЗНАЧИМА: FORGE_TELEGRAM_ANALYSIS_MODEL= это «не передавать --model вовсе», единственный способ вернуться к дефолту CLI, не редактируя telegram.toml.

validate_and_fix для путей анализа разворачивает ведущий ~ (expand_tilde: TOML не shell) и приводит prompts_path к АБСОЛЮТНОМУ — относительный превратился бы в РАЗНЫЕ файлы при разном рабочем каталоге процесса (devforge запускают и из репозитория, и из systemd).
