plugins/vk/src/media_cache.rs::CacheKey {peer_id: i64, message_id: i32, index: u8, kind: MediaKind} (Ф7).

ЕДИНСТВЕННЫЙ путь строк из URL в файловую систему: собрать CacheKey можно только через parse(peer, mid, idx, kind) -> Option<Self>, а он разбирает идентификаторы в числа. Всё, что не разбирается в i64 != 0 / i32 > 0 / u8 <= MAX_ATTACH_INDEX (9) / thumb|full, отбрасывается ДО обращения к диску.

Правила совпадают с остальными роутами плагина (dto::parse_dialog_id, routes::dialogs::parse_message_id) — есть тест message_id_rule_matches_the_route. Ведущий + отвергается: у одного вложения должно быть одно написание адреса, потому что ключ живёт ещё и во фронте (js/vk/format.js::mediaKey). Это осознанное расхождение с Telegram, где +42 нормализуется.

МЕТОДЫ: id() -> "<peer>/<mid>/<idx>/<kind>" (совпадает с фронтовым mediaKey и служит ключом media_inflight), message_key() -> (peer_id, message_id) — ключ ВТОРОГО уровня дедупа, index и kind в него не входят намеренно, peer_id_string(), stem() -> "<mid>.<idx>.<kind>".

Соответствие «ключ дедупа <-> имя файла» покрыто file_id_of_inflight и тестом inflight_key_translates_into_a_file_name: разъедься оно, sweeper начал бы удалять файлы прямо во время скачивания.