# Cargo.toml::reqwest

Phase 3 — HTTP-клиент reqwest 0.12 (резолвится в 0.12.28). Добавлен в tmux-web/Cargo.toml для proxy-запросов на удалённые devforge-инстансы. Features: json (RequestBuilder::json / Response::json), stream (стриминг body — пригодится для WS-прокси в Phase 4), rustls-tls (TLS без OpenSSL). default-features = false, чтобы НЕ тянуть default-tls (нужно для Homebrew bottle на macOS без системного OpenSSL). Используется в модуле remote_proxy.rs и в AppState.http (reqwest::Client — cheap-clonable Arc-обёртка).
