//! Bearer-аутентификация для remote-mode сервера.
//!
//! ## Назначение
//!
//! Когда `devforge` запускается с `--remote` (Phase 1), сервер биндится на
//! публичный адрес (`0.0.0.0` по умолчанию) и требует от клиентов
//! предъявлять заголовок `Authorization: Bearer <token>`. Токен сохраняется
//! в `~/.config/forge/server_config.json` и совпадает со значением, которое
//! пользователь видит в banner'е при первом запуске remote-mode.
//!
//! В legacy localhost-режиме (`devforge run` без флагов) сервер по-прежнему
//! слушает `127.0.0.1` без аутентификации — middleware при `auth_token=None`
//! просто пропускает запросы (full passthrough).
//!
//! ## Контракт middleware
//!
//! - **Path-исключения** (всегда пропускаются без проверки токена):
//!   - `GET /healthz` — health-check используется frontend'ом ДО получения
//!     токена (чтобы прочитать `remote_mode` и показать UI логина).
//!   - `/` (index.html) и `/assets/*`, `/static/*` — статика обслуживается
//!     до middleware'а, но мы дублируем проверку на всякий случай.
//! - Защищённый путь без `Authorization: Bearer <expected>` → **401**.
//! - Защищённый путь с `Authorization: Bearer <wrong>` → **401**.
//! - Защищённый путь с правильным токеном → пропуск дальше.
//!
//! Сравнение токена через `subtle::ConstantTimeEq` НЕ используется — у нас
//! 64-hex случайный токен и timing-side-channel риск минимальный; зависимость
//! на новый crate ради этого не оправдана.
//!
//! ## Интеграция
//!
//! Middleware применяется через `axum::middleware::from_fn_with_state`
//! ТОЛЬКО когда `AppState.auth_token = Some(_)`. См. `main.rs` (задача
//! ypp1.5).

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

/// Пути, которые пропускаются без проверки Bearer-токена. Дополняются
/// проверкой статических префиксов в [`is_path_excluded`].
const EXCLUDED_EXACT: &[&str] = &["/healthz", "/"];

/// Префиксы статики — пропускаются без аутентификации.
const EXCLUDED_PREFIXES: &[&str] = &["/assets/", "/static/"];

/// Параметры, нужные middleware'у. Передаются через
/// `State<AuthState>` чтобы не тащить `AppState` целиком (это упрощает
/// unit-тесты и устраняет циклическую зависимость auth ↔ main).
///
/// `auth_token = None` ⇒ middleware пропускает всё (legacy localhost). Это
/// инвариант: главный код подключает middleware только если token=Some, но
/// passthrough-ветка остаётся как defense-in-depth.
#[derive(Clone)]
pub struct AuthState {
    pub auth_token: Arc<Option<String>>,
}

impl AuthState {
    /// Конструктор для удобства тестов и интеграции в main.rs.
    pub fn new(token: Option<String>) -> Self {
        Self {
            auth_token: Arc::new(token),
        }
    }
}

/// Возвращает `true`, если запрос к этому пути НЕ требует Bearer-проверки.
pub fn is_path_excluded(path: &str) -> bool {
    if EXCLUDED_EXACT.contains(&path) {
        return true;
    }
    for p in EXCLUDED_PREFIXES {
        if path.starts_with(p) {
            return true;
        }
    }
    false
}

/// Извлекает Bearer-token из заголовка `Authorization`. Возвращает `None`,
/// если заголовка нет, формат не «Bearer <token>», или token пустой.
fn extract_bearer(req: &Request<Body>) -> Option<String> {
    let h = req.headers().get(axum::http::header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if !lower.starts_with("bearer ") {
        return None;
    }
    let token = s[7..].trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some(token)
}

/// Axum middleware: Bearer auth.
///
/// - При `auth_token = None` пропускает всё (passthrough).
/// - Для путей из [`is_path_excluded`] — пропускает без проверки.
/// - Иначе требует `Authorization: Bearer <auth_token>` или 401.
pub async fn bearer_auth(
    State(state): State<AuthState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Legacy localhost-mode: middleware-обёртка установлена, но токен пуст —
    // пропускаем всё. Это invariant защиты от случайной интеграции в legacy.
    let expected = match state.auth_token.as_ref() {
        Some(t) => t.clone(),
        None => return next.run(req).await,
    };

    let path = req.uri().path();
    if is_path_excluded(path) {
        return next.run(req).await;
    }

    match extract_bearer(&req) {
        Some(provided) if provided == expected => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Bearer realm=\"devforge\"")],
            "unauthorized",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Method, Request, StatusCode},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use tower::util::ServiceExt;

    /// Минимальный handler для тестов — возвращает 200 OK.
    async fn ok_handler() -> &'static str {
        "ok"
    }

    /// Собирает Router с auth middleware и тестовыми маршрутами.
    fn app(token: Option<&str>) -> Router {
        let state = AuthState::new(token.map(|s| s.to_string()));
        Router::new()
            .route("/healthz", get(ok_handler))
            .route("/api/projects", get(ok_handler))
            .route("/assets/app.js", get(ok_handler))
            .route("/", get(ok_handler))
            .layer(from_fn_with_state(state, bearer_auth))
    }

    fn req(method: Method, uri: &str, auth: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(a) = auth {
            b = b.header(header::AUTHORIZATION, a);
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn none_token_passes_everything() {
        let app = app(None);
        // Любой путь без заголовка → 200.
        for path in ["/healthz", "/api/projects", "/assets/app.js", "/"] {
            let resp = app
                .clone()
                .oneshot(req(Method::GET, path, None))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "path {path} should pass without token"
            );
        }
    }

    #[tokio::test]
    async fn some_token_healthz_pass_without_header() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(Method::GET, "/healthz", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn some_token_protected_without_header_returns_401() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(Method::GET, "/api/projects", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn some_token_protected_with_correct_bearer_passes() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("Bearer secret-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn some_token_protected_with_wrong_bearer_returns_401() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("Bearer WRONG-TOKEN"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn assets_path_excluded() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(Method::GET, "/assets/app.js", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_authorization_header_rejected() {
        let app = app(Some("secret-token"));
        // Не Bearer-схема.
        let resp = app
            .clone()
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("Basic dXNlcjpwYXNz"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Bearer без значения.
        let resp = app
            .clone()
            .oneshot(req(Method::GET, "/api/projects", Some("Bearer ")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn is_path_excluded_matrix() {
        assert!(is_path_excluded("/healthz"));
        assert!(is_path_excluded("/"));
        assert!(is_path_excluded("/assets/app.js"));
        assert!(is_path_excluded("/static/index.html"));
        assert!(!is_path_excluded("/api/projects"));
        assert!(!is_path_excluded("/ws/attach"));
        // Подстрока, не префикс — не excluded.
        assert!(!is_path_excluded("/api/assets/foo"));
    }

    // =========================================================================
    // Phase 8 .5 — Auth edge cases
    // =========================================================================

    #[tokio::test]
    async fn bearer_scheme_lowercase_accepted() {
        // RFC 7235 §2.1: scheme case-insensitive.
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("bearer secret-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "lowercase 'bearer' OK");
    }

    #[tokio::test]
    async fn bearer_scheme_uppercase_accepted() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("BEARER secret-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_scheme_mixed_case_accepted() {
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("BeArEr secret-token"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_ascii_authorization_header_rejected_without_panic() {
        // HeaderValue::from_str отвергает символы вне ASCII-printable.
        // Если кто-то всё-таки сконструирует через from_bytes — h.to_str()
        // вернёт Err и extract_bearer вернёт None.
        let app = app(Some("secret-token"));
        // Конструируем через from_bytes — байты валидные для HeaderValue,
        // но не валидный UTF-8 для to_str().
        let mut b = Request::builder()
            .method(Method::GET)
            .uri("/api/projects");
        b = b.header(
            header::AUTHORIZATION,
            axum::http::HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap(),
        );
        let resp = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn multiple_authorization_headers_use_first() {
        // axum/hyper: header().get() возвращает первое значение для multi-value
        // заголовков. Тест-as-spec: первый берётся, второй игнорируется.
        let app = app(Some("secret-token"));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/projects")
            .header(header::AUTHORIZATION, "Bearer secret-token")
            .header(header::AUTHORIZATION, "Bearer WRONG")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "первый Authorization берётся, второй — игнорируется"
        );
    }

    #[tokio::test]
    async fn static_index_html_excluded_without_token() {
        // /static/index.html попадает под /static/ префикс.
        let state = AuthState::new(Some("secret".to_string()));
        let router = Router::new()
            .route("/static/index.html", get(ok_handler))
            .layer(from_fn_with_state(state, bearer_auth));
        let resp = router
            .oneshot(req(Method::GET, "/static/index.html", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_uppercase_is_case_sensitive_requires_token() {
        // /HEALTHZ ≠ /healthz: middleware case-sensitive по path.
        // Если бы axum матчил /HEALTHZ к route("/healthz") — это вернуло бы
        // 200 (т.к. excluded). Текущее поведение: 404 (роута нет) — он не
        // вызывает middleware. Альтернативно: явно регистрируем /HEALTHZ
        // и проверяем 401.
        let state = AuthState::new(Some("secret".to_string()));
        let router = Router::new()
            .route("/HEALTHZ", get(ok_handler))
            .layer(from_fn_with_state(state, bearer_auth));
        let resp = router
            .oneshot(req(Method::GET, "/HEALTHZ", None))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "/HEALTHZ — case-sensitive, не в excluded-list → 401"
        );
    }

    #[tokio::test]
    async fn ws_upgrade_without_auth_returns_401_not_426() {
        // GET /ws/attach с Upgrade: websocket, но без Authorization → 401
        // (middleware отвечает раньше, чем axum::extract::WebSocketUpgrade
        // успеет вернуть 426).
        let state = AuthState::new(Some("secret".to_string()));
        let router = Router::new()
            .route("/ws/attach", get(ok_handler))
            .layer(from_fn_with_state(state, bearer_auth));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/ws/attach")
            .header("upgrade", "websocket")
            .header("connection", "upgrade")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "WS upgrade без Authorization → 401, не 426"
        );
    }

    #[tokio::test]
    async fn www_authenticate_header_present_in_401() {
        // RFC 6750 §3: 401 ответ ДОЛЖЕН содержать WWW-Authenticate: Bearer ...
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(Method::GET, "/api/projects", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let www = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            www.to_ascii_lowercase().contains("bearer"),
            "WWW-Authenticate должен содержать 'Bearer', got: {www:?}"
        );
    }

    #[tokio::test]
    async fn bearer_with_trailing_whitespace_in_token_accepted() {
        // extract_bearer делает trim — пробелы вокруг токена не ломают match.
        let app = app(Some("secret-token"));
        let resp = app
            .oneshot(req(
                Method::GET,
                "/api/projects",
                Some("Bearer   secret-token   "),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
