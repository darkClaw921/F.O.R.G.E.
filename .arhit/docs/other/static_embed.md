# static_embed

Модуль tmux-web/src/static_embed.rs — embedded-static fallback для axum-роутера devforge.

Что делает:
Встраивает содержимое каталога tmux-web/static/ (index.html, app.js, hotkeys.js, style.css и т.д.) непосредственно в бинарь на этапе компиляции через rust-embed (feature interpolate-folder-path) и отдаёт их через axum-handler с корректным Content-Type. Полностью устраняет зависимость от cwd или каталога рядом с исполняемым файлом — бинарь self-contained и работает откуда угодно (Homebrew, cargo install, /tmp).

Ключевые элементы:
- pub struct StaticAssets — #[derive(RustEmbed)] #[folder = "\$CARGO_MANIFEST_DIR/static/"]. \$CARGO_MANIFEST_DIR резолвится в путь до tmux-web/Cargo.toml, поэтому фактический folder — tmux-web/static/. pub чтобы при необходимости обращаться к ассетам из других модулей.
- pub async fn serve_static(uri: Uri) -> Response — axum Handler. Логика: trim_start_matches('/'); пустой путь → index.html; trailing-slash → <path>index.html (повторяет поведение ServeDir::append_index_html_on_directories); ищет в StaticAssets::get(&path); 200 + Content-Type (mime_guess::from_path) или 404 Not Found.

Зависимости (Cargo.toml):
- rust-embed = { version = "8", features = ["interpolate-folder-path"] } — без feature \$CARGO_MANIFEST_DIR не работает.
- mime_guess = "2" — определение Content-Type.

Связи:
- Подключается в crate::main как mod static_embed (после mod pty).
- Используется в Router как .fallback(static_embed::serve_static) — после .with_state(app_state), перед .layer(TraceLayer::new_for_http()).
- Заменил собой удалённые fn resolve_static_dir() и use tower_http::services::ServeDir.

Пример запроса:
- GET / → 200 text/html, тело = static/index.html
- GET /app.js → 200 text/javascript, тело = static/app.js
- GET /unknown → 404 Not Found
