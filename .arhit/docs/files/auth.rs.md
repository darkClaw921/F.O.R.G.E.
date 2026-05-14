# auth.rs

Bearer-authentication middleware для remote-mode сервера (Phase 1).

## Назначение
Когда devforge запускается с --remote, сервер биндится на публичный адрес и требует Authorization: Bearer <token>. В legacy localhost-режиме middleware не подключается вообще (см. main.rs: layer добавляется только при auth_token=Some).

## Структуры
- AuthState { auth_token: Arc<Option<String>> } — параметры middleware, передаются через State.
- AuthState::new(token: Option<String>) — конструктор.

## Функции
- bearer_auth(State<AuthState>, Request<Body>, Next) -> Response — axum 0.7 middleware. Path-исключения через is_path_excluded. На защищённом пути требует Bearer-token совпадающий с auth_token; иначе 401 с заголовком WWW-Authenticate: Bearer.
- is_path_excluded(path) -> bool — проверка path-exclusion.
- extract_bearer(req) -> Option<String> — парсит Authorization header.

## Path exclusions
- Точные: /healthz, / (root index.html)
- Префиксы: /assets/, /static/ — статика идёт без auth, чтобы браузер мог загрузить app.js до того, как пользователь ввёл токен.
- WS endpoints (/ws/*) аутентифицируются через handshake: Bearer-заголовок прокидывается браузером в Authorization (в нашем случае браузер не умеет, поэтому WS использует тот же Bearer в query или обрабатывается на TLS-уровне через reverse proxy).

## Зависимости
- axum 0.7 (middleware::Next, body::Body)
- tower (только для тестов: ServiceExt::oneshot)

## Тесты (8 unit)
passthrough_none_token, healthz_no_header_200, no_header_401, correct_bearer_200, wrong_bearer_401, assets_excluded, malformed_header (Basic, Bearer без значения), is_path_excluded_matrix.

## Интеграция
Подключается в main.rs через axum::middleware::from_fn_with_state ТОЛЬКО когда AppState.auth_token=Some. См. ypp1.5.

## Безопасность (Phase 7 smoke)
- Token НЕ попадает в DOM/window через GET /api/remote-servers (DTO RemoteServerView без token).
- Запрос с Authorization: Bearer <wrong> → 401.
- Запрос без Authorization на защищённый путь → 401.
- /healthz возвращает 200 даже без токена (intentional — UI должен прочитать remote_mode/version).
