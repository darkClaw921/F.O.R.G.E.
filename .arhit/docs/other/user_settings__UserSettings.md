# user_settings::UserSettings

Настройки уровня пользователя (~/.forge/user_settings.json). Все поля #[serde(default)] — частичный/пустой файл валиден.

Группа авто-апрува (добавлена в tw-pg5, см. auto_approve.rs):
- auto_approve_enabled: bool, default false — фича целиком (opt-in; при false воркер не делает tmux-вызовов);
- auto_approve_broad: bool, default false — выбирать «Yes, and don't ask again» / «Allow all actions for this session», где такой пункт есть;
- auto_approve_questions: bool, default TRUE (default_auto_approve_questions) — отвечать первым вариантом на AskUserQuestion; дефолт включённый, потому что флаг живёт внутри уже opt-in фичи и нужен для ВЫключения;
- auto_approve_blocklist: Vec<String>, default пустой — пользовательские стоп-слова, ДОПОЛНЯЮТ auto_approve::DEFAULT_DANGEROUS. PATCH-семантика — полная замена списка (merge не позволил бы снять одну запись); санитайзер sanitize_blocklist (trim, lowercase, дедуп, ≤200 символов, ≤128 записей) — отдельный от sanitize_visible_list, потому что стоп-слово содержит пробелы и спецсимволы командной строки.

Остальные группы: todo_* (дефолты карточек), echo_* (модель и нотификации Echo), cmd_hints_enabled, next_step_enabled, telegram_* и vk_* (хоткеи и белые списки видимости).

Читается воркерами каждый тик через state.user_settings.get() — переключение тумблера в UI действует сразу, без рестарта.
