# remotes

Реестр remote-серверов devforge (tmux-web/src/remotes.rs).

## Структуры
- RemoteServer{id, label, url, token} — внутреннее представление. КАСТОМНЫЙ Debug — token заменяется на '[REDACTED]'.
- RemoteServerView{id, label, url} — DTO для API (БЕЗ token).
- RemoteServerStore{file_path, servers: Vec<RemoteServer>} — in-memory + JSON-файл.

## Файл ~/.config/forge/remote_servers.json
Envelope: {servers: [RemoteServer...]}. Атомарный save через tmp + rename.

## API
- load(PathBuf) → Result<Self>. Отсутствующий файл → пустой store. Broken JSON → Err.
- save() → Result<()>. Создаёт каталог при отсутствии.
- list() / list_views() / get(id) — чтение.
- add(label, url, token) → Result<RemoteServer>. Slug из label через projects::slugify + дедупликация (-2/-3/...).
- remove(id) → bool. update(id, label?, token?) → Option<RemoteServer>.

## Контракт (Phase 8 .8)
- Slug collision: до 100+ дубликатов label → office, office-2, ..., office-100 (детерминированно).
- Unicode-only label ('Офис') → Err (пустой slug после ASCII-фильтра).
- Очень длинный label (500 chars) → принимается, slug может быть длинным, всё ASCII-safe.
- Debug на RemoteServer и RemoteServerStore НЕ светит token — [REDACTED].
- view_excludes_token — JSON-сериализация RemoteServerView не содержит token field.
- Broken JSON в файле → Err с file path в message, без паники.
- URL валидация (is_valid_remote_url): http://* / https://* OK, остальное — Err.
- URL trimming: trailing slash снимается при add.

## Тесты
17 unit-тестов в remotes::tests (Phase 2 + Phase 8 .8).
