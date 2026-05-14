# remote_proxy

HTTP/WS-прокси на удалённые devforge-инстансы (tmux-web/src/remote_proxy.rs).

## Назначение
Локальный devforge в режиме aggregator (--remote) принимает запросы с ?server=<id> и проксирует их на remote devforge через HTTP (reqwest) или WebSocket (tokio-tungstenite).

## Публичный API
- proxy_request(store, client, server_id, method, path, query, content_type, body) -> Result<(StatusCode, HeaderMap, Bytes), ProxyError> — основной HTTP-helper. Получает url+token из RemoteServerStore::get, добавляет Authorization: Bearer <token>, фильтрует hop-by-hop в ответе.
- proxy_websocket(store, server_id, upstream_path, query, downstream) -> Result<(), ProxyError> — generic WS-прокси (Phase 4). connect_async к remote с Bearer-заголовком, tokio::select! на pump между axum::WebSocket и tungstenite.
- enrich_with_origin(value, server_id) — добавляет поле origin=<server_id> в JSON-array элементы (Object) или сам Object.
- ProxyError { UnknownServer, InvalidUrl, Network, WebSocket }, ProxyError::into_response() мапит на (StatusCode, String): 404/500/502/502.

## Hop-by-hop фильтр
is_hop_by_hop() возвращает true для: connection, keep-alive, proxy-authenticate, proxy-authorization, te, trailers, transfer-encoding, upgrade. Эти заголовки НЕ форвардятся клиенту (RFC 7230 §6.1).

## Bearer injection
proxy_request пишет Authorization: Bearer <token>, где token — приватное поле RemoteServer{token} из RemoteServerStore. Заголовки клиента НЕ форвардятся upstream (reqwest сам не копирует их с axum-стороны).

## Тесты (Phase 8 .1)
Интеграционные тесты через wiremock в src/remote_proxy.rs tests:
- proxy_request_passes_200_ok / _204_no_content / _404_not_found / _500_server_error — статус mapping.
- proxy_request_timeout_returns_network_error — таймаут через ResponseTemplate::set_delay → ProxyError::Network(is_timeout).
- proxy_request_connection_refused_returns_network_error — bind на свободный порт, drop, connect → ProxyError::Network(is_connect).
- proxy_request_dns_fail_returns_network_error — .invalid TLD (RFC 6761).
- proxy_request_content_length_zero_empty_body — Content-Length: 0 + пустое тело.
- proxy_request_chunked_body_collected — крупное тело (~12 KB).
- proxy_request_does_not_follow_redirect — 302 проходит насквозь (reqwest::redirect::Policy::none).
- proxy_request_streams_large_body_10mb — 10 MB без OOM.

## Зависимости
- reqwest (HTTP-клиент, rustls-tls, redirect=none на прокси-стороне)
- tokio-tungstenite 0.24 (WS-клиент с rustls-tls-webpki-roots)
- futures-util (SinkExt/StreamExt для pump'а)
- crate::remotes::RemoteServerStore — источник url+token
- bytes, axum, serde_json
