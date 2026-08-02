# dialog_key

plugins/telegram/src/state.rs — свободная функция: dialog_id-строка -> ключ серверных кэшей.

Сигнатура: dialog_key(dialog_id: &str) -> Option<i64>.

Реализована через dto::parse_dialog_id, поэтому мусор и значения вне диапазонов Bot API
отсеиваются (None), а полученный ключ ПОБИТОВО совпадает с тем dialog_id, что уезжает во фронт
в JSON. Один разбор — один источник истины; собственный parse::<i64>() здесь развёл бы форматы.

Используется TelegramState::remember_dialogs (засев кэшей из списка DialogDto) и всеми путями,
где на входе строковый id, а на выходе нужен ключ notify_overrides / archived_ids.
