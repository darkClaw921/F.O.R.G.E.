# cli::run_pair

Реализация подкоманды devforge pair --generate (Phase 2).

## Назначение
Генерирует 64-hex Bearer-token и сохраняет его в ~/.config/forge/server_config.json. НЕ запускает сервер. Печатает инструкцию пользователю, чтобы он мог зарегистрировать этот сервер на локальном devforge.

## Алгоритм
1. Резолвит путь через crate::server_config::default_server_config_path().
2. Создаёт родительский каталог если нужно.
3. Генерирует токен через crate::server_config::generate_token_64hex() (2x UUID v4 → 64 hex символа).
4. Загружает существующий server_config.json через load_from():
   - Если файл есть — мерджит: auth_token переписывается, bind/port заполняются дефолтами (0.0.0.0, 7331) если пусты.
   - Если файла нет — создаёт ServerConfig { auth_token: Some(token), bind: Some('0.0.0.0'), port: Some(7331) }.
5. Atomic save через crate::server_config::save_to() (tempfile + rename).
6. Печатает банер: generated token / bind / port / instructions / saved path.

## CLI парсинг
- Грамматика: 'devforge pair --generate' или 'devforge pair -g'.
- Разбирается отдельным parse_pair() в cli.rs (вызывается из parse_from при first arg == 'pair').
- Без --generate возвращает ошибку.

## Поведение
- Идемпотентно — повторный запуск переписывает токен без подтверждения.
- Не запускает сервер — главный main() выходит сразу после выполнения.
- Если файл server_config.json повреждён — load_from() вернёт Err, run_pair тоже вернёт Err.

## Зависит от
- crate::server_config (generate_token_64hex, default_server_config_path, load_from, save_to, ServerConfig).
- anyhow для error context.
