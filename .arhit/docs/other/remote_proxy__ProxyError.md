# remote_proxy::ProxyError

Типизированные ошибки прокси-прохода (HTTP + WS).

## Варианты
- **UnknownServer(String)** — server_id отсутствует в RemoteServerStore. → 404 Not Found.
- **InvalidUrl(String)** — невалидный URL/header. → 500 Internal Server Error.
- **Network(reqwest::Error)** — сетевая ошибка HTTP-проходов (timeout, refused, TLS). → 502 Bad Gateway.
- **WebSocket(String)** — Phase 4. Ошибка WebSocket handshake / connect_async / tungstenite. → 502 Bad Gateway. Хранит текст оригинальной ошибки, чтобы не тянуть tungstenite::Error в публичный API.

## API
- impl Display — человеко-читаемый текст.
- impl std::error::Error — source() для Network варианта (chain).
- into_response() → (axum::StatusCode, String) — для axum-handler'ов возвращающих Result<_, (StatusCode, String)>.

## Используется
- HTTP proxy: try_proxy_to_remote, remote_server_healthz, remote_proxy::proxy_request.
- WS proxy: remote_proxy::proxy_websocket.
