# cli.rs

Модуль cli.rs — парсинг и диспатч CLI команд devforge.

## Назначение
Точка входа CLI. Парсит argv в RunOptions / Subcommand, диспатчит команды (run/pair/remote), выводит help/version.

## Структуры
- RunOptions { port, remote, bind, token } — параметры запуска сервера (Phase 1).
- Subcommand { Run(RunOptions), Pair(PairOptions), Remote(RemoteOptions), Help, Version } — диспатч.
- PairOptions { generate: bool } — для devforge pair.
- RemoteOptions { Add(...) , List, Remove(id) } — для devforge remote.

## Ключевые функции
- parse_from(argv: &[String]) → Result<Subcommand> — главный энтри-поинт. Делегирует в parse_run / parse_pair / parse_remote.
- parse_run() / parse_pair() / parse_remote() / parse_remote_add() — sub-parsers.
- print_help() / print_version() — stdout-выводы.
- state_dir() → Result<PathBuf> — резолвит ~/.config/forge (XDG).
- ensure_state_dir() — создаёт каталог если нет.
- run_pair(opts) — реализация devforge pair --generate (создаёт server_config.json с токеном).
- run_remote(opts) — реализация devforge remote list/add/remove (работает с RemoteServerStore напрямую без HTTP).
- derive_label_from_url(url) — выводит label из host'а URL для 'remote add' без --label.

## Phase 1 — флаги
- --port <N> — порт (default 8787).
- --remote — включить remote-mode (bind на bind/0.0.0.0, требовать токен).
- --bind <ADDR> — адрес bind'а (default 127.0.0.1 в legacy, 0.0.0.0 в --remote).
- --token <HEX> — Bearer-токен (или через env DEVFORGE_AUTH_TOKEN).

## Phase 2 — devforge pair / remote
- devforge pair --generate — генерит 64-hex токен и сохраняет в server_config.json (auth_token, bind=0.0.0.0). Печатает банер с инструкцией.
- devforge remote list / ls — табличный вывод реестра ID|LABEL|URL (без token).
- devforge remote add <URL> --token <HEX> [--label NAME] — добавляет запись через RemoteServerStore::add.
- devforge remote remove <ID> / rm / delete — удаляет.

## Архитектура
- CLI команды remote/pair работают НАПРЯМУЮ с файлами (~/.config/forge/*.json) — НЕ через REST API. Это нужно, чтобы команды работали даже когда сервер не запущен.
- parse_remote() диспатчит на parse_remote_add() при первом arg == 'add'. Поддерживает обе формы: --token=value и --token value.

## Зависит от
- crate::server_config — для PairOptions.
- crate::remotes::RemoteServerStore — для remote-команд.
- anyhow, std::env, std::fs, std::path.
