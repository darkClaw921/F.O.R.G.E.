# TelegramConfig

Конфигурация плагина forge-telegram (plugins/telegram/src/config.rs).

ЧТО ЭТО: cheap-clonable структура со всеми параметрами вкладки Telegram. Поля: api_id: Option<i32> / api_hash: Option<String> (креды приложения с my.telegram.org; Option, потому что пользователь вводит их в UI-wizard'е, и конфиг обязан быть валидным без них), session_path (~/.config/forge/telegram.session — файл MTProto-сессии, фактически ключ от переписки), cache_dir (~/.forge/telegram-cache), max_upload_bytes (2 GiB), max_cache_bytes (512 MiB), catch_up (догонять пропущенные апдейты), update_queue_limit (1024).

ЦЕПОЧКА ЗАГРУЗКИ (эталон — EchoConfig): load() = default() -> load_from_file(default_config_path()) -> apply_env() -> validate_and_fix(). load() НИКОГДА не паникует и не возвращает ошибку: нет файла — штатный путь (креды вводятся в UI), битый TOML/мусор в env — warn-лог и дефолт.

МЕТОДЫ:
- load_from_file(&Path) — TOML через toml 0.8; NotFound = debug-лог + дефолты.
- apply_env() — FORGE_TELEGRAM_{API_ID,API_HASH,SESSION_PATH,CACHE_DIR,MAX_UPLOAD_BYTES,MAX_CACHE_BYTES,CATCH_UP,UPDATE_QUEUE_LIMIT}; нечисловые/небулевые значения игнорируются с warn.
- validate() -> anyhow::Result<()> — чистая проверка: api_hash непуст если задан, max_upload_bytes > 0, update_queue_limit > 0. Вызывается из forge_telegram::init; ошибка = хост стартует без плагина.
- validate_and_fix() — чинит невалидные значения дефолтами + ensure_dirs().
- ensure_dirs() — create_dir_all для parent(session_path) и cache_dir.
- has_credentials() — есть api_id И непустой api_hash; из этого LoginFlow::from_config выводит начальное состояние (Disconnected vs NotConfigured).
- save_credentials(api_id, api_hash) — сигнатура зафиксирована заранее; реализация (atomic tmp+rename, chmod 600) приходит вместе с login-flow, сейчас честно возвращает Err и ничего не пишет.

СВЯЗИ: используется forge_telegram::init -> TelegramState::new; ре-экспортируется как forge_telegram::TelegramConfig и вызывается из tmux-web/src/main.rs (TelegramConfig::load()). 10 unit-тестов покрывают дефолты, TOML, приоритет env над TOML, мусор в env, validate и заглушку save_credentials.
