# UserSettings

user_settings.rs — глобальные настройки уровня пользователя, файл ~/.forge/user_settings.json. Все поля #[serde(default)], чтобы старый/пустой/неполный файл грузился без миграции.

Поля: todo_default_plan_mode, todo_default_priority (clamp 0..=4), todo_default_issue_type, todo_plan_mode_suffix, todo_confirm_delete, todo_confirm_promote_on_drag, echo_default_model (Option, пустая строка = сброс в None), echo_notifications_enabled (default true), cmd_hints_enabled (default FALSE, opt-in), next_step_enabled (default FALSE, opt-in и дорогая — дёргает Claude CLI), telegram_hotkeys_enabled (default TRUE), telegram_hotkeys (BTreeMap<String,String>, default пустая), telegram_filter_enabled (default FALSE), telegram_visible_folders / telegram_visible_dialogs (Vec<String>, default пустые), vk_filter_enabled (default FALSE), vk_visible_kinds / vk_visible_peers (Vec<String>, default пустые).

ПРО ДЕФОЛТЫ. cmd_hints_enabled и next_step_enabled намеренно нарушают инвариант нулевой конфигурации (раньше обе фичи были всегда включены) — это прямое требование пользователя, чинить обратно на true НЕЛЬЗЯ. А telegram_hotkeys_enabled = true инвариант НЕ нарушает: сама вкладка Telegram уже opt-in (флаг --telegram), хоткеи живут только внутри неё и молчат в полях ввода, и пользователь просил именно тумблер для выключения работающей фичи. Тест test_telegram_hotkeys_default_enabled_with_empty_map фиксирует это, чтобы правку 'привести к общему виду' ловил CI, а не пользователь. Оба фильтра видимости (telegram_filter_enabled, vk_filter_enabled) дефолтятся в FALSE: нулевая конфигурация обязана показывать все чаты и беседы, фронт читает их строго как === true.

БЕЛЫЙ СПИСОК VK (Ф10 плагина VK). vk_visible_kinds — ТИПЫ бесед 'user' | 'chat' | 'community', а НЕ ключи четырёх серверных фильтров VK (all/unread/important/unanswered): те являются параметром запроса messages.getConversations, а не свойством беседы, и 'показывать только непрочитанные' как постоянная настройка означало бы исчезновение беседы из панели в момент её прочтения. Это единственное место, где словарь настроек VK намеренно расходится с телеграмным. vk_visible_peers — peer_id отдельных бесед строками. Фильтр считается ДЕЙСТВУЮЩИМ только при поднятом тумблере И непустом хотя бы одном списке (инвариант 'включён не равно действует' — см. [[vk-visibility-model]]).

PATCH (PatchUserSettingsReq) применяет только Some(..)-варианты. telegram_hotkeys, оба телеграмных списка и оба списка vk_* заменяются ЦЕЛИКОМ, а не сливаются: только так UI может снять галочку (прислать список без записи), пустой список = 'ничего не выбрано'. Значения проходят через sanitize_hotkeys / sanitize_visible_list. Выключение тумблера НЕ стирает выбор — включив обратно, пользователь получает свой список.

ЛИМИТЫ ОБЩИЕ ДЛЯ ОБЕИХ ПАНЕЛЕЙ: MAX_TELEGRAM_VISIBLE = 512 записей на список, MAX_VISIBLE_ENTRY_LEN = 64 символа на запись, MAX_TELEGRAM_HOTKEYS = 64, MAX_HOTKEY_LEN = 64. Исторические имена оставлены намеренно — переименование тронуло бы работающий код Telegram.

ТОКЕНОВ МЕССЕНДЖЕРОВ ЗДЕСЬ НЕТ И БЫТЬ НЕ ДОЛЖНО: токен VK живёт в ~/.config/forge/vk.toml (0600), а user_settings.json читается фронтендом целиком через GET /api/user-settings.

Persistence: atomic save (запись в <path>.tmp + rename). Файл НЕ создаётся при чтении — только при первом patch.

Тесты VK: test_vk_visibility_defaults_off_and_empty, test_vk_visibility_patch_replaces_lists, test_vk_visibility_patch_sanitizes_and_keeps_telegram_intact, test_vk_visible_peers_capped.
