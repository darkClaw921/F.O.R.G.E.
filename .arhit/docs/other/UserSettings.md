# UserSettings

user_settings.rs — глобальные настройки уровня пользователя, файл ~/.forge/user_settings.json. Все поля #[serde(default)], чтобы старый/пустой/неполный файл грузился без миграции.

Поля: todo_default_plan_mode, todo_default_priority (clamp 0..=4), todo_default_issue_type, todo_plan_mode_suffix, todo_confirm_delete, todo_confirm_promote_on_drag, echo_default_model (Option, пустая строка = сброс в None), echo_notifications_enabled (default true), cmd_hints_enabled (default FALSE, opt-in), next_step_enabled (default FALSE, opt-in и дорогая — дёргает Claude CLI), telegram_hotkeys_enabled (default TRUE), telegram_hotkeys (BTreeMap<String,String>, default пустая).

ПРО ДЕФОЛТЫ. cmd_hints_enabled и next_step_enabled намеренно нарушают инвариант нулевой конфигурации (раньше обе фичи были всегда включены) — это прямое требование пользователя, чинить обратно на true НЕЛЬЗЯ. А telegram_hotkeys_enabled = true инвариант НЕ нарушает: сама вкладка Telegram уже opt-in (флаг --telegram), хоткеи живут только внутри неё и молчат в полях ввода, и пользователь просил именно тумблер для выключения работающей фичи. Тест test_telegram_hotkeys_default_enabled_with_empty_map фиксирует это, чтобы правку 'привести к общему виду' ловил CI, а не пользователь.

PATCH (PatchUserSettingsReq) применяет только Some(..)-варианты. telegram_hotkeys заменяется ЦЕЛИКОМ, а не сливается: только так UI может снять одно переопределение (прислать карту без него), пустая карта = сброс всех действий к дефолтам. Значение проходит через sanitize_hotkeys. Выключение тумблера НЕ стирает переназначения — включив обратно, пользователь получает свои клавиши.

Persistence: atomic save (запись в <path>.tmp + rename). Файл НЕ создаётся при чтении — только при первом patch.
