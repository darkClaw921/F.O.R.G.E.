# cli

CLI парсер devforge (tmux-web/src/cli.rs).

## Команды
- devforge run [--port N] [--remote] [--bind <addr>] [--token <hex>]
- devforge start (background daemon) / stop / status
- devforge pair --generate (или -g) — генерация Bearer-токена в server_config.json
- devforge remote list/add/remove

## API
- parse() / parse_from(Vec<String>) → Result<Mode>
- help_text() → String
- run_pair(&PairOptions) — реализация pair-команды
- run_remote(&RemoteCmd) — реализация remote-команд
- state_dir() / pid_path() / log_path()

## Контракт (закреплён тестами Phase 8 .7)
- --token без --remote принимается парсером (server_config::resolve игнорирует token если remote_mode=false).
- --bind '[::1]:8080' — bracketed IPv6 принимается как строка, валидация TCP-listener'ом.
- --port range: 1..=65535 valid, 0 → Err, негативные → Err.
- Unknown top-level cmd → Err с 'unknown' в сообщении.
- pair --generate и pair -g — алиасы. Двойной --generate -g OK (boolean OR).
- pair extra positional → Err.
- remote add дубликат label → slug -2/-3 suffix (не Err — store::add).
- run_pair (finalize_token) идемпотентен: существующий токен в файле preserved (НЕ rotation). Будущая force-rotation policy потребует --force флаг.

## Тесты
35 unit-тестов в cli::tests (Phase 1 + Phase 8 .7).
