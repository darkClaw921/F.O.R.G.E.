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

/// Защищённые префиксы — требуют Bearer-токена. Всё, что не попадает под
/// эти префиксы (и не /healthz / /), считается статикой и пропускается.
/// Это нужно потому что embedded static-asset endpoint (`static_embed::
/// serve_static`) отдаёт файлы прямо из корня (`/app.js`, `/style.css`,
/// `/quick-cmd.js`, `/hotkeys.js`, ...) — без явного префикса. Иначе
/// браузер на телефоне получит 401 на `/style.css` и UI не загрузится.
const PROTECTED_PREFIXES: &[&str] = &["/api/", "/ws/"];

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
///
/// Логика: защищаются только пути из [`PROTECTED_PREFIXES`] (`/api/`, `/ws/`).
/// Остальное — статика (`/`, `/style.css`, `/app.js`, `/quick-cmd.js`,
/// `/hotkeys.js`, `/favicon.ico`, ...) — отдаётся без токена, иначе клиент
/// (особенно мобильный, открывший сразу URL без Authorization-header) не
/// получит даже HTML/CSS и UI не загрузится.
pub fn is_path_excluded(path: &str) -> bool {
    if EXCLUDED_EXACT.contains(&path) {
        return true;
    }
    for p in EXCLUDED_PREFIXES {
        if path.starts_with(p) {
            return true;
        }
    }
    for p in PROTECTED_PREFIXES {
        if path.starts_with(p) {
            return false;
        }
    }
    // Не /api/ и не /ws/ — это статика, пропускаем.
    true
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

/// Извлекает токен из URL query (`?token=...`). Нужно для WebSocket-эндпоинтов:
/// браузер не позволяет ставить кастомные headers на WS из JS, поэтому
/// токен передаётся в query. Возвращает `None`, если параметра нет или он пуст.
fn extract_query_token(req: &Request<Body>) -> Option<String> {
    let query = req.uri().query()?;
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k != "token" {
            continue;
        }
        let v = it.next().unwrap_or("");
        let decoded = urlencoding_decode(v);
        if decoded.is_empty() {
            return None;
        }
        return Some(decoded);
    }
    None
}

/// Минимальный URL-decode для значения `?token=...` (только `%XX` и `+`).
/// Свой код вместо crate `urlencoding` — избегаем новой зависимости ради
/// одного места.
fn urlencoding_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            let parsed = h.and_then(|s| u8::from_str_radix(s, 16).ok());
            if let Some(byte) = parsed {
                out.push(byte as char);
                i += 3;
            } else {
                out.push(b as char);
                i += 1;
            }
        } else {
            out.push(b as char);
            i += 1;
        }
    }
    out
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

    // Для WS-эндпоинтов разрешаем токен через query (?token=...), т.к.
    // браузер не позволяет ставить custom headers на WebSocket из JS.
    let provided_header = extract_bearer(&req);
    let provided_query = if path.starts_with("/ws/") {
        extract_query_token(&req)
    } else {
        None
    };

    let provided = provided_header.or(provided_query);
    match provided {
        Some(token) if token == expected => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Bearer realm=\"devforge\"")],
            "unauthorized",
        )
            .into_response(),
    }
}

// =============================================================================
// CSRF / drive-by guard (forge-cgzf)
// =============================================================================
//
// В дефолтном localhost-режиме bearer_auth не подключается вообще
// (auth_token=None), а WebSocket-апгрейд и мутирующие REST-эндпоинты не
// проверяли Origin. Любой вредоносный сайт, открытый в браузере пользователя,
// мог:
//   - открыть `ws://127.0.0.1:7331/ws/attach?session=X` и писать команды в
//     shell через PTY tmux attach (drive-by RCE);
//   - сделать `fetch('http://127.0.0.1:7331/api/todos', {method:'POST', ...})`
//     с `Content-Type: text/plain` — простой запрос без CORS-preflight,
//     который браузер отправит cross-origin, → promote → tmux send-keys Enter
//     в shell-панель.
//
// `csrf_guard` подключается ВСЕГДА (и в localhost, и в remote) внешним слоем и:
//   1) для `/ws/*` — требует, чтобы `Origin` (если он есть) совпадал с `Host`.
//      Браузер ВСЕГДА шлёт Origin на WebSocket, поэтому cross-origin drive-by
//      отбивается. Native-клиенты (CLI, мобильное приложение) Origin не шлют —
//      их пропускаем.
//   2) для мутирующих `/api/*` (POST/PUT/PATCH/DELETE) — требует same-origin
//      `Origin` (если он присутствует) И `Content-Type: application/json`.
//      Это лишает атакующего «простого запроса» (form/text/plain без
//      preflight): браузер обязан сделать CORS-preflight, который мы не
//      разрешаем (нет CORS-заголовков в ответах), и реальный мутирующий
//      запрос не уйдёт.
//      ЕДИНСТВЕННОЕ исключение по Content-Type — raw-заливка файла
//      (`POST /api/telegram/upload` и `POST /api/vk/upload`) с
//      `application/octet-stream`: этот Content-Type тоже не входит в
//      «простые», то есть preflight для него обязателен, а
//      same-origin-проверка остаётся в силе. Подробности — в
//      `is_allowed_raw_body`.

/// Извлекает `host:port` (authority) из значения заголовка `Origin`.
/// `Origin` имеет вид `scheme://host[:port]`. Возвращает `None` для `null`
/// (sandboxed iframe / file://) и для синтаксически кривых значений.
fn origin_authority(origin: &str) -> Option<&str> {
    let o = origin.trim();
    if o.is_empty() || o.eq_ignore_ascii_case("null") {
        return None;
    }
    let rest = o.split_once("://").map(|(_, r)| r)?;
    // authority заканчивается на первом '/', '?' или '#' (которых в Origin
    // обычно нет, но отрезаем для надёжности).
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

/// Проверяет, что Origin (если задан) принадлежит тому же хосту, что и Host.
/// Возвращает:
/// - `true`, если Origin отсутствует (native-клиент) ИЛИ совпадает с Host;
/// - `false`, если Origin задан и НЕ совпадает с Host (cross-origin).
fn is_same_origin(req: &Request<Body>) -> bool {
    let origin = match req.headers().get(axum::http::header::ORIGIN) {
        Some(v) => match v.to_str() {
            Ok(s) => s,
            Err(_) => return false, // не-ASCII Origin — отвергаем
        },
        None => return true, // нет Origin → не браузерный cross-origin запрос
    };
    let origin_auth = match origin_authority(origin) {
        Some(a) => a,
        None => return false, // "null" / кривой Origin → отвергаем
    };
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    if host.is_empty() {
        return false;
    }
    origin_auth.eq_ignore_ascii_case(host)
}

/// Проверяет `Content-Type: application/json` (с возможным `; charset=...`).
fn is_json_content_type(req: &Request<Body>) -> bool {
    req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            let main = ct.split(';').next().unwrap_or("").trim();
            main.eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

/// Точные пути raw-заливки файла в плагинах.
///
/// Сравнение по ПОЛНОМУ пути, а не по префиксу `/api/telegram/` или
/// `/api/vk/`: под префиксное исключение автоматически попадали бы все будущие
/// роуты плагина, и однажды кто-то добавил бы мутирующий роут, который молча
/// потерял бы защиту Content-Type.
///
/// Поэтому список растёт ЯВНО, по одной строке на роут: добавление второго
/// плагина не имеет права ослабить правило, ради которого оно написано.
const RAW_UPLOAD_PATHS: [&str; 3] = [
    "/api/telegram/upload",
    "/api/vk/upload",
    "/api/bitrix/upload",
];

/// Content-Type, на котором работает raw-загрузка файла.
const RAW_UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";

/// Путь вставки файла в сессию — `/api/sessions/<имя>/files`.
///
/// Имя сессии в пути произвольное, поэтому точным сравнением (как для
/// плагинных роутов выше) такой путь не описать. Правило нарочно жёсткое:
/// после `/api/sessions/` ровно два сегмента, второй — `files`, имя сессии
/// непустое. Остальные роуты `/api/sessions/...` (windows, worktree, history)
/// под него не попадают, так что исключение покрывает ровно один эндпоинт.
///
/// Реальный MIME файла едет в query (`?mime=`), а НЕ в Content-Type: иначе
/// вставка текстового файла ушла бы с `text/plain`, а это «простой» для CORS
/// Content-Type — и исключение превратилось бы в дыру в CSRF-защите.
fn is_session_file_upload_path(path: &str) -> bool {
    let rest = match path.strip_prefix("/api/sessions/") {
        Some(r) => r,
        None => return false,
    };
    match rest.rsplit_once('/') {
        Some((name, "files")) => !name.is_empty() && !name.contains('/'),
        _ => false,
    }
}

/// Разрешено ли этому запросу нести тело без `application/json`.
///
/// Единственное исключение — raw-заливка файла (`POST /api/telegram/upload`,
/// `POST /api/vk/upload`) с `Content-Type: application/octet-stream`: файл
/// передаётся сырыми байтами (см. `plugins/telegram/src/routes/upload.rs` и
/// `plugins/vk/src/routes/upload.rs`), и JSON здесь физически неприменим.
///
/// ПОЧЕМУ ЭТО НЕ ОСЛАБЛЯЕТ ЗАЩИТУ. `application/octet-stream` НЕ входит в
/// список Content-Type «простых запросов» CORS (там только
/// `application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`).
/// Значит, кросс-доменный `fetch`/XHR с таким Content-Type браузер обязан
/// предварить preflight-запросом `OPTIONS`, а мы не отдаём CORS-заголовков
/// вообще — preflight не пройдёт, и реальный запрос не уйдёт. Через
/// `<form>` такой Content-Type выставить нельзя в принципе. Проверка
/// same-origin (`is_same_origin`) при этом остаётся ОБЯЗАТЕЛЬНОЙ и
/// выполняется до вызова этой функции.
fn is_allowed_raw_body(req: &Request<Body>) -> bool {
    let path = req.uri().path();
    if !RAW_UPLOAD_PATHS.contains(&path) && !is_session_file_upload_path(path) {
        return false;
    }
    req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            let main = ct.split(';').next().unwrap_or("").trim();
            main.eq_ignore_ascii_case(RAW_UPLOAD_CONTENT_TYPE)
        })
        .unwrap_or(false)
}

/// Возвращает `true`, если у запроса есть тело (по заголовкам, до его чтения).
/// Учитываем `Content-Length > 0` и `Transfer-Encoding` (chunked). Bodyless
/// мутирующие запросы (например `POST /select`, `DELETE /api/...`) тело не
/// несут — для них JSON-Content-Type не требуем (CSRF-вектора через тело нет,
/// same-origin-проверка уже отработала).
fn has_request_body(req: &Request<Body>) -> bool {
    if req.headers().contains_key(axum::http::header::TRANSFER_ENCODING) {
        return true;
    }
    req.headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Возвращает `true`, если HTTP-метод мутирующий (меняет состояние сервера).
fn is_mutating_method(method: &axum::http::Method) -> bool {
    matches!(
        *method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
}

/// Axum middleware: anti-CSRF / drive-by guard. Подключается ВСЕГДА (см.
/// `main.rs`), независимо от remote/localhost режима.
///
/// Логика:
/// - `/ws/*` — если `Origin` задан и не same-origin → 403; иначе пропуск.
/// - мутирующий `/api/*` — требует same-origin Origin (если задан) И
///   `Content-Type: application/json` → иначе 403.
/// - всё остальное (GET/HEAD, статика, healthz) — пропуск.
pub async fn csrf_guard(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path();
    let method = req.method().clone();

    // WebSocket-апгрейд: блокируем cross-origin (drive-by RCE через /ws/attach).
    if path.starts_with("/ws/") {
        if !is_same_origin(&req) {
            tracing::warn!(
                path,
                origin = ?req.headers().get(axum::http::header::ORIGIN),
                "csrf_guard: rejected cross-origin WebSocket upgrade"
            );
            return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
        }
        return next.run(req).await;
    }

    // Мутирующие REST `/api/*`: требуем same-origin + JSON Content-Type.
    if path.starts_with("/api/") && is_mutating_method(&method) {
        if !is_same_origin(&req) {
            tracing::warn!(
                path,
                %method,
                origin = ?req.headers().get(axum::http::header::ORIGIN),
                "csrf_guard: rejected cross-origin mutating request"
            );
            return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
        }
        // Тело есть → обязателен application/json. Это лишает атакующего
        // «простого запроса» (text/plain, form-urlencoded, multipart): такой
        // cross-origin запрос с JSON-Content-Type браузер обязан предварить
        // CORS-preflight'ом, который мы не разрешаем.
        //
        // Единственное исключение — raw-загрузка файла в Telegram-плагине
        // (см. `is_allowed_raw_body`): её Content-Type тоже не «простой»,
        // поэтому preflight обязателен и для неё.
        if has_request_body(&req) && !is_json_content_type(&req) && !is_allowed_raw_body(&req) {
            tracing::warn!(
                path,
                %method,
                content_type = ?req.headers().get(axum::http::header::CONTENT_TYPE),
                "csrf_guard: rejected mutating request with body but without application/json Content-Type"
            );
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type: application/json required",
            )
                .into_response();
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Method, Request, StatusCode},
        middleware::{from_fn, from_fn_with_state},
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

    // -------------------------------------------------------------------------
    // csrf_guard (forge-cgzf)
    // -------------------------------------------------------------------------

    #[test]
    fn origin_authority_parsing() {
        assert_eq!(
            origin_authority("http://127.0.0.1:7331"),
            Some("127.0.0.1:7331")
        );
        assert_eq!(
            origin_authority("https://evil.example.com"),
            Some("evil.example.com")
        );
        assert_eq!(origin_authority("null"), None);
        assert_eq!(origin_authority(""), None);
        assert_eq!(origin_authority("garbage-no-scheme"), None);
        assert_eq!(
            origin_authority("http://host:9/path?x"),
            Some("host:9")
        );
    }

    /// Собирает router с ВСЕГДА-включённым csrf_guard и тестовыми маршрутами.
    fn csrf_app() -> Router {
        use axum::routing::{delete, post};
        Router::new()
            .route("/ws/attach", get(ok_handler))
            .route("/api/todos", post(ok_handler))
            .route("/api/todos/:id", delete(ok_handler))
            .route("/api/sessions", get(ok_handler))
            // Роуты плагинов: их регистрируют сами плагины, но для проверки
            // csrf_guard достаточно одноимённых заглушек.
            .route("/api/telegram/upload", post(ok_handler))
            .route("/api/telegram/dialogs/:id/messages", post(ok_handler))
            .route("/api/vk/upload", post(ok_handler))
            .route("/api/vk/dialogs/:id/messages", post(ok_handler))
            .route("/api/bitrix/upload", post(ok_handler))
            .route("/api/bitrix/dialogs/:id/messages", post(ok_handler))
            // Вставка файла из буфера в терминал сессии (см.
            // `is_session_file_upload_path`) — соседний `windows` намеренно
            // рядом, чтобы тест ловил расширение исключения на другие роуты.
            .route("/api/sessions/:name/files", post(ok_handler))
            .route("/api/sessions/:name/windows", post(ok_handler))
            .layer(from_fn(csrf_guard))
    }

    /// Собирает запрос для csrf-тестов. `body_len` имитирует Content-Length,
    /// который в реальном HTTP проставляет клиент/hyper (в unit-тесте
    /// `Request::builder().body()` его не выставляет).
    fn csrf_req(
        method: Method,
        uri: &str,
        origin: Option<&str>,
        host: Option<&str>,
        content_type: Option<&str>,
        body: &'static str,
    ) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        if let Some(ct) = content_type {
            b = b.header(header::CONTENT_TYPE, ct);
        }
        if !body.is_empty() {
            b = b.header(header::CONTENT_LENGTH, body.len());
        }
        b.body(Body::from(body)).unwrap()
    }

    #[tokio::test]
    async fn ws_cross_origin_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::GET,
                "/ws/attach",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ws_same_origin_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::GET,
                "/ws/attach",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ws_no_origin_allowed_native_client() {
        // CLI/native клиент не шлёт Origin — drive-by невозможен, пропускаем.
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::GET,
                "/ws/attach",
                None,
                Some("127.0.0.1:7331"),
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mutating_cross_origin_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/todos",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                Some("application/json"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn mutating_with_body_non_json_rejected() {
        // Cross-origin «простой запрос» text/plain без preflight — блок.
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/todos",
                None,
                Some("127.0.0.1:7331"),
                Some("text/plain"),
                "hello",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn mutating_same_origin_json_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/todos",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/json; charset=utf-8"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bodyless_delete_no_content_type_allowed() {
        // Bodyless DELETE без Content-Type — легитимный фронтовый паттерн.
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::DELETE,
                "/api/todos/42",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // --- raw-загрузка файла в Telegram (forge-tg-phase5-lzhj.3) ---

    /// Same-origin `POST /api/telegram/upload` с octet-stream проходит:
    /// JSON для сырых байтов файла неприменим.
    #[tokio::test]
    async fn telegram_upload_octet_stream_same_origin_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/telegram/upload",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // --- вставка файла в терминал сессии (forge-660j) ---

    /// Same-origin `POST /api/sessions/<имя>/files` с octet-stream проходит:
    /// вставленный из буфера файл едет сырыми байтами.
    #[tokio::test]
    async fn session_file_upload_octet_stream_same_origin_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/sessions/dev/files?name=image.png&mime=image%2Fpng",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00PNG",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Исключение НЕ отменяет same-origin и для вставки файла.
    #[tokio::test]
    async fn session_file_upload_cross_origin_still_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/sessions/dev/files",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00PNG",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// «Простой» для CORS Content-Type не проходит даже на разрешённом пути:
    /// именно поэтому настоящий MIME файла едет в query, а не в заголовке.
    #[tokio::test]
    async fn session_file_upload_text_plain_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/sessions/dev/files?mime=text%2Fplain",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("text/plain"),
                "hello",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Исключение точечное: соседний роут сессии с octet-stream по-прежнему
    /// требует JSON.
    #[tokio::test]
    async fn other_session_routes_still_require_json() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/sessions/dev/windows",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Чистая проверка предиката пути — без HTTP-обвязки.
    #[test]
    fn session_file_upload_path_predicate() {
        assert!(is_session_file_upload_path("/api/sessions/dev/files"));
        assert!(is_session_file_upload_path(
            "/api/sessions/my-session_1/files"
        ));
        assert!(!is_session_file_upload_path("/api/sessions/files"));
        assert!(!is_session_file_upload_path("/api/sessions//files"));
        assert!(!is_session_file_upload_path("/api/sessions/dev/windows"));
        assert!(!is_session_file_upload_path(
            "/api/sessions/dev/files/extra"
        ));
        assert!(!is_session_file_upload_path("/api/telegram/files"));
    }

    /// Исключение НЕ отменяет same-origin: чужой Origin по-прежнему 403.
    #[tokio::test]
    async fn telegram_upload_cross_origin_still_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/telegram/upload",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// Исключение точечное по ПУТИ: соседний мутирующий роут плагина с
    /// octet-stream по-прежнему отвергается.
    #[tokio::test]
    async fn other_telegram_routes_still_require_json() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/telegram/dialogs/1/messages",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Исключение точечное по CONTENT-TYPE: «простой» text/plain на том же
    /// пути отвергается (иначе появился бы CSRF-вектор без preflight).
    #[tokio::test]
    async fn telegram_upload_with_simple_content_type_rejected() {
        for ct in ["text/plain", "multipart/form-data", "application/x-www-form-urlencoded"] {
            let app = csrf_app();
            let resp = app
                .oneshot(csrf_req(
                    Method::POST,
                    "/api/telegram/upload",
                    Some("http://127.0.0.1:7331"),
                    Some("127.0.0.1:7331"),
                    Some(ct),
                    "hello",
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
        }
    }

    /// Параметры Content-Type не мешают: `octet-stream; charset=binary`
    /// разбирается по основному типу.
    #[tokio::test]
    async fn telegram_upload_content_type_parameters_are_ignored() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/telegram/upload",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("Application/Octet-Stream; charset=binary"),
                "data",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // --- raw-загрузка файла в VK (forge-vk-f8-2pox.4) ---
    //
    // Плагинов с raw-телом стало два, и это ровно тот момент, когда «сделаем
    // префиксное исключение по /api/vk/» выглядит соблазнительно. Тесты ниже
    // фиксируют, почему нельзя: список путей растёт явно, по одной строке на
    // роут, и соседний мутирующий роут ТОГО ЖЕ плагина защиту не теряет.

    /// Same-origin `POST /api/vk/upload` с octet-stream проходит.
    #[tokio::test]
    async fn vk_upload_octet_stream_same_origin_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/vk/upload",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Исключение НЕ отменяет same-origin и для VK.
    #[tokio::test]
    async fn vk_upload_cross_origin_still_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/vk/upload",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// ГЛАВНЫЙ тест правки: сравнение осталось ТОЧНЫМ по полному пути.
    ///
    /// Мутация «сравнивать по префиксу `/api/vk/`» краснеет здесь: отправка
    /// сообщения — мутирующий роут того же плагина, и защиту по Content-Type
    /// она терять не имеет права.
    #[tokio::test]
    async fn other_vk_routes_still_require_json() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/vk/dialogs/1234/messages",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// И по CONTENT-TYPE исключение тоже точечное: «простые» типы отвергаются,
    /// иначе появился бы CSRF-вектор без preflight.
    #[tokio::test]
    async fn vk_upload_with_simple_content_type_rejected() {
        for ct in [
            "text/plain",
            "multipart/form-data",
            "application/x-www-form-urlencoded",
        ] {
            let app = csrf_app();
            let resp = app
                .oneshot(csrf_req(
                    Method::POST,
                    "/api/vk/upload",
                    Some("http://127.0.0.1:7331"),
                    Some("127.0.0.1:7331"),
                    Some(ct),
                    "hello",
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
        }
    }

    /// Пути, ПОХОЖИЕ на разрешённый, исключением не являются: ни хвост, ни
    /// префикс, ни соседний сегмент.
    #[tokio::test]
    async fn lookalike_upload_paths_are_not_exempt() {
        for uri in [
            "/api/vk/uploads",
            "/api/vk/upload/extra",
            "/api/vk/upload2",
            "/api/telegram/upload/x",
        ] {
            let app = Router::new()
                .route("/api/vk/uploads", axum::routing::post(ok_handler))
                .route("/api/vk/upload/extra", axum::routing::post(ok_handler))
                .route("/api/vk/upload2", axum::routing::post(ok_handler))
                .route("/api/telegram/upload/x", axum::routing::post(ok_handler))
                .layer(from_fn(csrf_guard));
            let resp = app
                .oneshot(csrf_req(
                    Method::POST,
                    uri,
                    Some("http://127.0.0.1:7331"),
                    Some("127.0.0.1:7331"),
                    Some("application/octet-stream"),
                    "data",
                ))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{uri}: сравнение обязано быть точным по полному пути"
            );
        }
    }

    // --- raw-загрузка файла в Bitrix24 (forge-b24-f1-twlx.7) ---
    //
    // Третий плагин с raw-телом. Соблазн заменить список префиксом стал ещё
    // сильнее — тесты ниже фиксируют, почему нельзя.

    /// Same-origin `POST /api/bitrix/upload` с octet-stream проходит.
    #[tokio::test]
    async fn bitrix_upload_octet_stream_same_origin_allowed() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/bitrix/upload",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Исключение НЕ отменяет same-origin и для Bitrix24.
    #[tokio::test]
    async fn bitrix_upload_cross_origin_still_rejected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/bitrix/upload",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "\x00\x01binary",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// Сравнение осталось ТОЧНЫМ по полному пути: соседний мутирующий роут
    /// того же плагина по-прежнему требует `application/json`.
    #[tokio::test]
    async fn other_bitrix_routes_still_require_json() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::POST,
                "/api/bitrix/dialogs/chat7/messages",
                Some("http://127.0.0.1:7331"),
                Some("127.0.0.1:7331"),
                Some("application/octet-stream"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Пути, ПОХОЖИЕ на `/api/bitrix/upload`, исключением не являются.
    #[tokio::test]
    async fn bitrix_lookalike_upload_paths_are_not_exempt() {
        for uri in ["/api/bitrix/uploads", "/api/bitrix/upload/extra"] {
            let app = Router::new()
                .route("/api/bitrix/uploads", axum::routing::post(ok_handler))
                .route("/api/bitrix/upload/extra", axum::routing::post(ok_handler))
                .layer(from_fn(csrf_guard));
            let resp = app
                .oneshot(csrf_req(
                    Method::POST,
                    uri,
                    Some("http://127.0.0.1:7331"),
                    Some("127.0.0.1:7331"),
                    Some("application/octet-stream"),
                    "data",
                ))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{uri}: сравнение обязано быть точным по полному пути"
            );
        }
    }

    #[tokio::test]
    async fn get_request_not_affected() {
        let app = csrf_app();
        let resp = app
            .oneshot(csrf_req(
                Method::GET,
                "/api/sessions",
                Some("http://evil.example.com"),
                Some("127.0.0.1:7331"),
                None,
                "",
            ))
            .await
            .unwrap();
        // GET — не мутирующий, Origin не проверяем (read-only, защита на чтении
        // не нужна и сломала бы навигацию). Пропуск.
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
