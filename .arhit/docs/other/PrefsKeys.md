# PrefsKeys

Таблица имён полей одной панели в user_settings.json (plugins/voice/src/prefs.rs). Константы VK, TELEGRAM, BITRIX.

Поля таблицы: autotranscribe (тумблер авторазбора), filter_enabled (тумблер белого списка видимости), visible_kinds / visible_peers (сам список видимости), scope_kinds / scope_peers (белый список АВТОРАЗБОРА, добавлен фазой forge-cvscope-f1).

## Почему таблица, а не format!("{prefix}_…")

Словарь у трёх панелей РАЗНЫЙ: vk_visible_kinds против telegram_visible_folders, vk_voice_scope_peers против telegram_voice_scope_dialogs. Собранное из префикса имя разошлось бы с хостом МОЛЧА — тумблер и скоуп просто перестали бы действовать, без единой ошибки. Поэтому имена перечислены буквально, а тест prefs::tests::every_key_exists_in_the_host_source читает исходник tmux-web/src/user_settings.rs и проверяет, что каждое имя там есть.

## Значения

VK: vk_voice_autotranscribe, vk_filter_enabled, vk_visible_kinds, vk_visible_peers, vk_voice_scope_kinds, vk_voice_scope_peers.
TELEGRAM: telegram_voice_autotranscribe, telegram_filter_enabled, telegram_visible_folders, telegram_visible_dialogs, telegram_voice_scope_folders, telegram_voice_scope_dialogs («виды» у Telegram — это ПАПКИ аккаунта).
BITRIX: bitrix_voice_autotranscribe, bitrix_filter_enabled, bitrix_visible_kinds, bitrix_visible_peers, bitrix_voice_scope_kinds, bitrix_voice_scope_peers.

Читается таблица функцией prefs::read(path, keys), которая НИКОГДА не падает: нет файла, битый JSON, чужие типы в полях — всё это Prefs::default() (авторазбор выключен, списки пусты).
