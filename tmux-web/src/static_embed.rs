//! Embedded-static модуль — отдаёт содержимое `tmux-web/static/` напрямую
//! из бинаря, без зависимости от cwd или каталога рядом с исполняемым файлом.
//!
//! Используется через `rust-embed` (feature `interpolate-folder-path`, чтобы
//! `$CARGO_MANIFEST_DIR` разрешался относительно `tmux-web/Cargo.toml`, а не
//! workspace root). Все файлы из `static/` (index.html, app.js, hotkeys.js,
//! style.css и т.д.) пакуются в секцию данных бинаря на этапе компиляции.
//!
//! Подключается как axum-fallback в [`crate::main`]:
//!
//! ```ignore
//! .fallback(static_embed::serve_static)
//! ```
//!
//! Поведение [`serve_static`]:
//! - `GET /`              → отдаёт `index.html` (Content-Type: text/html)
//! - `GET /<path>`        → ищет `<path>` в embedded, отдаёт с mime-типом по
//!                           расширению (через `mime_guess`)
//! - `GET /<path>/` (slash на конце) → ищет `<path>/index.html`, что повторяет
//!                                       поведение `ServeDir::append_index_html_on_directories`
//! - Файл не найден       → HTTP 404 с пустым телом-сообщением

use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::Response,
};
use rust_embed::RustEmbed;

/// RustEmbed-структура, указывающая на каталог `static/` пакета `devforge`.
///
/// `$CARGO_MANIFEST_DIR` резолвится rust-embed во время компиляции в путь до
/// `tmux-web/`, поэтому фактический folder — `tmux-web/static/`. Это работает
/// независимо от workspace-структуры и не требует копирования каталога рядом
/// с бинарём при установке через Homebrew/cargo install.
///
/// `pub`, чтобы при необходимости можно было обращаться к ассетам из других
/// модулей (например, debug-эндпоинт, листинг файлов).
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/static/"]
pub struct StaticAssets;

/// Axum-handler для embedded-static.
///
/// Принимает запрошенный URI, отрезает ведущий `/`, нормализует пустой путь
/// и trailing-slash к `index.html`, ищет файл в [`StaticAssets`] и отдаёт его
/// с корректным `Content-Type`. Если файла нет — отвечает `404 Not Found`.
///
/// # Контракты
///
/// - Возвращает `Response<Body>` (не `Result`), чтобы axum принял функцию
///   как `Handler` через `.fallback(serve_static)`.
/// - `Response::builder().unwrap()` безопасен: мы конструируем валидный
///   `Content-Type` из `mime_guess` и стандартный статус.
pub async fn serve_static(uri: Uri) -> Response {
    let raw_path = uri.path().trim_start_matches('/');

    // Нормализация: пустой путь и trailing-slash → index.html в этой папке.
    let path: String = if raw_path.is_empty() {
        "index.html".to_string()
    } else if raw_path.ends_with('/') {
        format!("{raw_path}index.html")
    } else {
        raw_path.to_string()
    };

    match StaticAssets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref());
            // Service worker и manifest должны проверяться браузером каждый раз,
            // иначе update-flow PWA ненадёжен (старый sw.js залипает в HTTP-кэше).
            if path == "sw.js" || path == "manifest.webmanifest" {
                builder = builder.header(header::CACHE_CONTROL, "no-cache");
            }
            builder
                .body(Body::from(content.data.into_owned()))
                .expect("valid static response")
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .expect("valid 404 response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};

    /// Хелпер: вызвать serve_static и вернуть (status, content_type, cache_control,
    /// body). Заголовки читаются ДО into_body() (into_body поглощает Response).
    async fn call(uri: &str) -> (StatusCode, Option<String>, Option<String>, Vec<u8>) {
        let resp = serve_static(uri.parse::<Uri>().unwrap()).await;
        let status = resp.status();
        let ct = resp
            .headers()
            .get(CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string());
        let cc = resp
            .headers()
            .get(CACHE_CONTROL)
            .map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, ct, cc, body)
    }

    /// sw.js → 200 + Cache-Control: no-cache + javascript-mime + непустое тело.
    #[tokio::test]
    async fn sw_js_has_no_cache() {
        let (status, ct, cc, body) = call("/sw.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cc.as_deref(), Some("no-cache"));
        let ct = ct.expect("content-type present");
        assert!(
            ct.contains("javascript"),
            "sw.js должен быть javascript-mime, got: {ct}"
        );
        assert!(!body.is_empty(), "тело sw.js непустое");
    }

    /// manifest.webmanifest → 200 + Cache-Control: no-cache + непустое тело.
    /// mime ассертим нестрого (mime_guess может вернуть application/manifest+json).
    #[tokio::test]
    async fn manifest_has_no_cache() {
        let (status, _ct, cc, body) = call("/manifest.webmanifest").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cc.as_deref(), Some("no-cache"));
        assert!(!body.is_empty());
    }

    /// app.js — обычный файл → 200, БЕЗ Cache-Control, javascript-mime.
    #[tokio::test]
    async fn app_js_no_cache_control_header() {
        let (status, ct, cc, body) = call("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(cc.is_none(), "обычная статика без Cache-Control, got: {cc:?}");
        assert!(ct.unwrap().contains("javascript"));
        assert!(!body.is_empty());
    }

    /// style.css → 200, БЕЗ Cache-Control, text/css.
    #[tokio::test]
    async fn style_css_no_cache_and_css_mime() {
        let (status, ct, cc, _body) = call("/style.css").await;
        assert_eq!(status, StatusCode::OK);
        assert!(cc.is_none());
        assert!(
            ct.as_deref().unwrap().starts_with("text/css"),
            "got: {ct:?}"
        );
    }

    /// Отсутствующий путь → 404 + тело "404 Not Found" + без Cache-Control.
    #[tokio::test]
    async fn missing_path_is_404() {
        let (status, _ct, cc, body) = call("/does-not-exist-zzz.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, b"404 Not Found");
        assert!(cc.is_none());
    }

    /// Пустой путь "/" → index.html (200, text/html, без Cache-Control).
    #[tokio::test]
    async fn root_path_serves_index_html() {
        let (status, ct, cc, body) = call("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.as_deref().unwrap().starts_with("text/html"),
            "got: {ct:?}"
        );
        assert!(cc.is_none(), "index.html ≠ sw.js/manifest → без Cache-Control");
        assert!(!body.is_empty());
    }

    /// trailing slash на несуществующий каталог → ищет <dir>/index.html → 404.
    #[tokio::test]
    async fn trailing_slash_nonexistent_dir_is_404() {
        let (status, ..) = call("/nonexistdir-zzz/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// query-string не ломает резолюцию: /sw.js?v=123 → находит sw.js, no-cache.
    #[tokio::test]
    async fn query_string_does_not_break_lookup() {
        let (status, _ct, cc, body) = call("/sw.js?v=123").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cc.as_deref(), Some("no-cache"), "query не попадает в имя ассета");
        assert!(!body.is_empty());
    }

    /// no-cache применяется ТОЛЬКО к точному path=="sw.js"/"manifest.webmanifest"
    /// (строгое ==, не contains/ends_with). Вложенный путь, оканчивающийся на
    /// "sw.js", НЕ должен получить no-cache. Используем несуществующий путь:
    /// проверяем, что условие no-cache строгое — даже если бы файл существовал,
    /// path "js/sw.js" != "sw.js". Здесь файла нет → 404 без Cache-Control
    /// (404-ветка не выставляет заголовок), что доказывает: no-cache не
    /// «прилипает» к подстроке. Дополнительно проверяем реальный вложенный
    /// бинарный ассет ниже.
    #[tokio::test]
    async fn nested_swjs_like_path_not_no_cache() {
        // js/sw.js не существует → 404. Главное: путь != "sw.js" строго.
        let (status, _ct, cc, _body) = call("/js/sw.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(cc.is_none());
    }

    /// Бинарный вложенный ассет: icons/icon-192.png → 200, image/png,
    /// без Cache-Control, непустое бинарное тело.
    #[tokio::test]
    async fn nested_png_asset_image_mime_no_cache() {
        let (status, ct, cc, body) = call("/icons/icon-192.png").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct.as_deref(), Some("image/png"));
        assert!(cc.is_none(), "обычный бинарник без Cache-Control");
        assert!(!body.is_empty());
    }

    /// Идемпотентность: два вызова /sw.js дают идентичный результат
    /// (status, Cache-Control, длина тела). Embedded-ассеты read-only.
    #[tokio::test]
    async fn repeated_calls_are_deterministic() {
        let (s1, _c1, cc1, b1) = call("/sw.js").await;
        let (s2, _c2, cc2, b2) = call("/sw.js").await;
        assert_eq!(s1, s2);
        assert_eq!(cc1, cc2);
        assert_eq!(b1.len(), b2.len());
    }
}
