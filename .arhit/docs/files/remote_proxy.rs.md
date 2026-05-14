# remote_proxy.rs

HTTP- и WebSocket-прокси на удалённые devforge-инстансы. Расположение: tmux-web/src/remote_proxy.rs.

## Назначение
Когда локальный devforge запущен с --remote и в реестре RemoteServerStore есть записи о других серверах, фронтенд может попросить ресурс с конкретного remote через ?server=<id>. Локальный сервер выступает как прозрачный HTTP/WS-прокси.

## Публичный API

### ProxyError (enum)
- UnknownServer(String) → 404 NOT_FOUND
- InvalidUrl(String)    → 500 INTERNAL_SERVER_ERROR
- Network(reqwest::Error) → 502 BAD_GATEWAY
- WebSocket(String) (Phase 4) → 502 BAD_GATEWAY
Имеет into_response() для axum-handler'ов и From<reqwest::Error>.

### proxy_request(store, client, server_id, method, path, query, content_type, body) (Phase 3)
- Достаёт url+token из store, склеивает <url><path>?<query>.
- Делает reqwest запрос с Authorization: Bearer <token>.
- Фильтрует hop-by-hop headers (RFC 7230 §6.1).
- Возвращает (StatusCode, HeaderMap, Bytes).
- Phase 7: warn! на network/timeout ошибки (с is_timeout/is_connect флагами), trace! на non-2xx.

### enrich_with_origin(value, server_id) (Phase 3)
- Array → каждый Object-item получает origin=<server_id>.
- Object → origin в корне.
- Скаляры/null — no-op.
- Idempotent (перезаписывает существующий origin).

### proxy_websocket(store, server_id, upstream_path, query, downstream) (Phase 4)
Generic WS-прокси для /ws/attach, /ws/lazygit, /ws/tasks, /ws/todos.
Алгоритм:
1. Достаёт RemoteServer{url, token} из store.
2. Конвертирует http(s):// → ws(s):// (http_to_ws_url).
3. tokio_tungstenite::connect_async с Authorization: Bearer <token> (build_upstream_request).
4. Двунаправленный pump через tokio::join! (down_to_up, up_to_down).
5. Phase 7: trace! на каждый Close-frame с обеих сторон (code+reason+server_id+path).
На отсутствующий server / handshake-fail закрывает downstream с code 1011.

## Hop-by-hop фильтр
is_hop_by_hop() матчит: connection, keep-alive, proxy-authenticate, proxy-authorization, te, trailers, transfer-encoding, upgrade.

## Логирование (Phase 7)
- HTTP: tracing::warn! на network errors с server_id/path/error/is_timeout/is_connect; tracing::trace! на non-2xx с server_id/path/status.
- WS: tracing::trace! на upgrade-fail / Close-кадры / send-errors / recv-errors с server_id/path/code/reason.
Видны при RUST_LOG=devforge=trace.

## Unit-тесты (16+ штук)
enrich_*: array/object/string/number/null/mixed/empty/idempotent.
proxy_error_*: into_response для 404/500/502 (Network, WebSocket).
is_hop_by_hop_matrix.
proxy_request_unknown_server (smoke).
http_to_ws_url_* (http→ws, https→wss, passthrough).
build_upstream_request_adds_bearer / rejects_invalid_token.

## Зависимости
- reqwest 0.12 (rustls-tls, stream).
- tokio_tungstenite (WS client).
- axum 0.7 (downstream WebSocket).
- futures_util (SinkExt/StreamExt).
- bytes, serde_json.
- crate::remotes::RemoteServerStore.
