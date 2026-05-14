# /api/remote-servers

REST CRUD endpoints для реестра remote-серверов devforge (Phase 2).

## Регистрация
Эндпоинты регистрируются в Router ТОЛЬКО при remote_mode=true. В обычном (localhost) режиме обращение к /api/remote-servers вернёт 404 (fallback на статику). Логика регистрации в main.rs внутри блока 'if remote_mode {...}'.

## AppState
- remotes: Arc<RwLock<RemoteServerStore>> — общий store; cheap-clonable.
- Инициализируется в main() через remotes::default_remotes_path() + RemoteServerStore::load().

## Endpoints
### GET /api/remote-servers → 200 Json<Vec<RemoteServerView>>
list_remote_servers — возвращает массив { id, label, url }. Поле token НЕ включается (DTO RemoteServerView без token). Read-lock.

### POST /api/remote-servers → 201 Json<RemoteServerView>
create_remote_server — Тело { label, url, token }. Валидация в RemoteServerStore::add (label/url/token непустые, url начинается с http:// или https://). ID генерится через slugify(label) с авто-дедупликацией. После add вызывается store.save() (atomic). 400 при невалидном вводе.

### DELETE /api/remote-servers/:id → 204
delete_remote_server — удаление. 404 если id неизвестен. После remove — save().

### PATCH /api/remote-servers/:id → 200 Json<RemoteServerView>
patch_remote_server — Тело { label?, token? }. Опциональные поля. 404 если id неизвестен. id/url неизменяемы. После update — save().

### GET /api/remote-servers/:id/healthz → 501
remote_server_healthz — Заглушка для Phase 2. Возвращает 501 'Not Implemented'. Phase 3 заменит это на проксирование GET <url>/healthz через reqwest::Client с Bearer-токеном, возвращая { online, remote_mode, version }. Сейчас просто проверяет существование id (404 если нет).

## Безопасность
- Token никогда не появляется в API-ответах (DTO RemoteServerView).
- Все мутирующие операции под write-lock'ом.
- POST/PATCH вызывают atomic save (tempfile+rename).

## Зависит от
- remotes::RemoteServerStore — реализует store.
- AppState.remotes — Arc<RwLock<...>>.
- axum::Json, axum::extract::Path/State.
