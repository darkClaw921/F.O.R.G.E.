# remotes.rs

Модуль remotes.rs — реестр remote-серверов devforge (Phase 2).

## Назначение
Локальный devforge может агрегировать несколько remote-инстансов devforge (запущенных на других машинах с --remote). Этот модуль хранит список таких удалённых серверов: id, label, base URL и Bearer-token.
Файл персиста — ~/.config/forge/remote_servers.json (через cli::state_dir()).

## Структуры
- RemoteServer { id, label, url, token } — одна запись. Сериализуется как есть на диск; в API-ответы НЕ отдаётся (token утечёт).
- RemoteServerView { id, label, url } — public DTO без token. Используется в /api/remote-servers responses.
- RemoteServerStore — in-memory копия + путь к файлу. Не Clone намеренно; доступ из axum через Arc<RwLock<...>>.
- RemotesFile — приватный envelope { servers: Vec<RemoteServer> }.

## Ключевые методы RemoteServerStore
- load(path) — читает JSON; если файла нет — возвращает пустой store БЕЗ создания файла.
- save() — atomic write через tempfile + rename (паттерн projects.rs / server_config.rs).
- list() / list_views() / get(id) — read-only.
- add(label, url, token) → RemoteServer — генерит id через projects::slugify с авто-дедупликацией (-2, -3, ...). Валидирует: непустые label/url/token, url начинается с http:// или https://. Trim trailing slash в url.
- remove(id) → bool — удаляет, возвращает true если было.
- update(id, label?, token?) → Option<RemoteServer> — изменяет label/token; id и url неизменны.

## Свободные функции
- default_remotes_path() → ~/.config/forge/remote_servers.json.
- is_valid_remote_url(s) — проверка http://|https://.
- trim_trailing_slash(s) — нормализация URL.

## Бизнес-логика
- Реестр работает ВСЕГДА, независимо от remote_mode сервера. CLI 'devforge remote add/list/remove' работает даже при legacy localhost.
- REST CRUD /api/remote-servers подключается только в remote_mode (Phase 2 task .3).
- Token хранится только локально и используется проксированием в Phase 3-4. Никогда не утекает в API-ответы.
- Slugify-collision: ID стабилен (база — slug от первого label), при коллизии добавляется -2, -3, ...
- Atomic save: tempfile в той же папке, потом rename — защита от частичной записи при crash.

## Unit-тесты (11 шт.)
load_missing_returns_empty, save_load_roundtrip, add_then_remove, update_label_and_token_keeps_id, update_unknown_id_returns_none, slugify_collision_appends_suffix, add_rejects_invalid_input, add_trims_trailing_slash, view_excludes_token, atomic_save_via_tempfile_rename, is_valid_remote_url_matrix.

## Зависит от
- projects::slugify — генерация id.
- cli::state_dir() — путь к ~/.config/forge/.
- anyhow, serde, serde_json, tracing — стандартный стек.
