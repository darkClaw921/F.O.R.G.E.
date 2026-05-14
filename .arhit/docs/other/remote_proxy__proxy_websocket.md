# remote_proxy::proxy_websocket

Generic WebSocket-прокси для всех 4 endpoint'ов remote-devforge (/ws/attach, /ws/lazygit, /ws/tasks, /ws/todos).

## Сигнатура
```rust
pub async fn proxy_websocket(
    store: &RemoteServerStore,
    server_id: &str,
    upstream_path: &str,
    query: &str,
    downstream: WebSocket,
) -> Result<(), ProxyError>
```

## Поведение
1. Берёт RemoteServer{url, token} из store. Если id неизвестен — закрывает downstream с close{1011, 'unknown remote server'} и возвращает Err(UnknownServer).
2. Конвертирует http://→ws:// и https://→wss:// через http_to_ws_url.
3. Собирает Request через IntoClientRequest + добавляет 'Authorization: Bearer <token>' через build_upstream_request.
4. Открывает upstream через tokio_tungstenite::connect_async. На ошибку → close downstream с 1011 + Err(WebSocket).
5. split() обоих WS на (tx, rx) и крутит две задачи через tokio::join!:
   - down_to_up: читает axum::Message, конвертирует в tungstenite::Message через axum_to_tungstenite и шлёт в upstream. На Close или ошибку — посылает Close в upstream и завершается.
   - up_to_down: симметрично в обратную сторону через tungstenite_to_axum.
6. Когда один pump завершился, он шлёт Close в свою sink-сторону, что вызывает завершение второй задачи на следующей итерации (cascading close).
7. Все ошибки логируются через tracing::trace! (server_id, url, error).

## Конвертация фреймов
- axum_to_tungstenite: Text/Binary/Ping/Pong → 1:1; Close(None) → Close(None); Close(Some(cf)) → Close(Some) с конвертацией CloseCode через .into().
- tungstenite_to_axum: симметрично + Frame(f) → Binary(f.payload().to_vec()) как fallback (на практике reader не эмитит Frame).

## Зависимости
- tokio-tungstenite 0.24 с features connect/handshake/rustls-tls-webpki-roots.
- futures_util::{SinkExt, StreamExt} для split() и .next()/.send().
- axum::extract::ws::{Message, WebSocket, CloseFrame}.

## Используется
- ws::attach (Phase 4.3) — ?server=<id> → proxy_websocket(.., '/ws/attach', ..)
- ws::lazygit_attach (Phase 4.3) — ?server=<id> → proxy_websocket(.., '/ws/lazygit', ..)
- ws_tasks::tasks_ws (Phase 4.4)
- ws_todos::todos_ws (Phase 4.4)

## Ограничения
- Кастомный Authorization передаётся ТОЛЬКО в handshake — refresh tokens не предусмотрены.
- Reconnect-логика на caller'е (если нужно). Этот helper — однократный pump.
- TLS-валидация — через webpki-roots (Mozilla CA bundle). Self-signed certs пока не поддержаны.
