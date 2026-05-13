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
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.into_owned()))
                .expect("valid static response")
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .expect("valid 404 response"),
    }
}
