# tmux-web/src/tmux.rs::parse_session_line

Парсит строку из tmux list-sessions формата name|id|attached|windows|created|path|session_group. Использует splitn(7, '|'). Поля path и session_group опциональны (backward-compat со старым форматом 5/6 колонок). Пустой session_group мапится в None. Возвращает None если первые 5 колонок отсутствуют, числа не парсятся, или name/id пустые.
