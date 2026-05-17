# tmux-web/src/auth.rs

Bearer-auth middleware для remote-mode (tmux-web/src/auth.rs). Применяется через axum::middleware::from_fn_with_state ТОЛЬКО когда AppState.auth_token=Some.

## Контракт
- При auth_token=None: passthrough (legacy localhost).
- /healthz, / — exact-исключения (всегда без токена).
- /assets/*, /static/* — префиксы статики (legacy compat).
- /api/*, /ws/* — ЗАЩИЩЁННЫЕ префиксы (PROTECTED_PREFIXES).
- Всё прочее — статика (embedded static_embed::serve_static подаёт /app.js, /style.css, /quick-cmd.js, /hotkeys.js, /favicon.ico прямо из корня). Эти пути исключены автоматически, иначе мобильный клиент по QR-URL получит 401 на /style.css и UI не загрузится.

## is_path_excluded(path)
Возвращает true (НЕ требовать токен) если: path ∈ EXCLUDED_EXACT; path начинается с EXCLUDED_PREFIXES. Возвращает false если path начинается с PROTECTED_PREFIXES (/api/, /ws/). Иначе по умолчанию true (это статика).

## extract_bearer / extract_query_token
- extract_bearer(req): Authorization: Bearer <token>.
- extract_query_token(req): ?token=... в query. Используется ТОЛЬКО для /ws/* — браузер не позволяет ставить custom headers на WebSocket из JS, поэтому токен передаётся в query. urlencoding_decode — собственная мини-функция (без зависимостей) для %XX и +.

## bearer_auth middleware
1. Если auth_token=None → passthrough.
2. is_path_excluded → passthrough.
3. Для /ws/* пробует header И query, для остального — только header.
4. provided == expected → passthrough, иначе 401 + WWW-Authenticate: Bearer realm.

## Зависимости
- axum 0.7 (body/extract/http/middleware/response).
- Никаких новых crates (urlencoding_decode реализовано вручную).
