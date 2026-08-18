# bot/platforms.rs

Книга проб платформ forge-meet (Ф4). ProbeSet/ProbeBook: встроенный probes.toml через include_str, поэлементное переопределение пользовательским ~/.config/forge/meet.probes.toml (путь из FORGE_MEET_PROBES_PATH), 17 замороженных имён проб, массив кандидатов на каждую. probe_element_expression и observe_expression собирают выражения для Runtime.evaluate: исходник observe.js со снятым export плюс пробы ДАННЫМИ в JSON. Битый TOML не роняет плагин. Страж no_probe_selector_leaks_into_the_capture_sources доказывает, что запись не зависит от вёрстки.
