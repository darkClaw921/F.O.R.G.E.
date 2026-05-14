# auth

Bearer-аутентификация в remote-mode (tmux-web/src/auth.rs).

## API
- AuthState{auth_token: Arc<Option<String>>} — state для middleware.
- AuthState::new(Option<String>) — конструктор.
- is_path_excluded(path) — true для /healthz, /, /assets/*, /static/*.
- extract_bearer(&Request) — парсит Authorization, поддерживает Bearer/bearer/BEARER/BeArEr (case-insensitive), trim вокруг токена.
- bearer_auth() — axum middleware: auth_token=None passthrough; excluded paths passthrough; mismatch → 401 + WWW-Authenticate.

## Контракт
- Case-insensitive scheme: 'bearer X', 'BEARER X', 'BeArEr X' принимаются.
- WWW-Authenticate: Bearer realm="devforge" в каждом 401 (RFC 6750 §3).
- Multiple Authorization headers: используется ПЕРВЫЙ (axum default for HeaderMap::get).
- Path matching case-sensitive: /HEALTHZ ≠ /healthz (НЕ excluded, требует токен).
- /static/*, /assets/* — статика без auth.
- WS upgrade без Authorization → 401 (middleware отрабатывает ДО WebSocketUpgrade extractor).
- Non-ASCII Authorization → 401 (HeaderValue::to_str → Err → None → 401).

## Тесты (Phase 8 .5)
В src/auth.rs#tests — 18 тестов всего (9 новых):
- bearer_scheme_lowercase/uppercase/mixed_case_accepted.
- non_ascii_authorization_header_rejected_without_panic.
- multiple_authorization_headers_use_first.
- static_index_html_excluded_without_token.
- healthz_uppercase_is_case_sensitive_requires_token.
- ws_upgrade_without_auth_returns_401_not_426.
- www_authenticate_header_present_in_401.
- bearer_with_trailing_whitespace_in_token_accepted.
