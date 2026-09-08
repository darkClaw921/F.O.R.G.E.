# read_portal

forge_voice::prefs::read_portal(path, keys, Option<portal_id>) — чтение настроек панели для КОНКРЕТНОГО портала (Ф7.3 мультипортальности Битрикса).

СИГНАТУРА И СОВМЕСТИМОСТЬ. read(path, keys) сохранён и делегирует read_portal(..., None): VK и Telegram не тронуты вовсе — портал у них один по определению (аккаунт), в их PrefsKeys portal_prefs=None, и передача portal_id им ничего не меняет (тест the_other_panels_ignore_the_portal_argument).

ГДЕ ЛЕЖАТ КЛЮЧИ. PrefsKeys дополнена полем portal_prefs: Option<&str> — Some("bitrix_portal_prefs") только у BITRIX. Имена полей ВНУТРИ записи портала (filter_enabled, visible_kinds, visible_peers, voice_scope_kinds, voice_scope_peers) — константа PORTAL_FIELDS; они без префикса панели, потому что внутри записи портала префикс ничего не различает. Тест the_portal_record_fields_exist_in_the_host_source читает исходник хоста и требует буквального совпадения имён: разъехаться эти строки могут только МОЛЧА.

ПРАВИЛО ЧТЕНИЯ то же, что у хоста и фронта: запись портала -> пустая карта значит плоские ключи -> иначе дефолты (см. док bitrix_portal_prefs). Реализуется portal_record(): None означает «читай плоские».

ТУМБЛЕР ГЛОБАЛЬНЫЙ. autotranscribe читается из КОРНЯ файла при любом портале — он не ссылается ни на какой dialog_id. Скоуп при этом портальный.

КТО ЗОВЁТ: plugins/bitrix/src/voice_auto.rs::prefs(state) передаёт state.portal_id() — источник истины идентичности портала, а не config.portal_id.

СТОРОНА ОТКАЗА не изменилась: любой сбой (нет файла, битый JSON, чужие типы в карте) — это Prefs::default, то есть авторазбор выключен.
