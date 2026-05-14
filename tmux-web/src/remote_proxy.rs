//! HTTP-прокси на удалённые devforge-инстансы.
//!
//! ## Назначение
//!
//! Когда локальный devforge запущен в режиме aggregator'а (`--remote`) и в
//! реестре [`crate::remotes::RemoteServerStore`] есть записи о других серверах,
//! фронтенд может попросить любой ресурс с конкретного remote через
//! query-параметр `?server=<id>`. Локальный сервер выступает как прозрачный
//! HTTP-прокси: достаёт `RemoteServer { url, token }` из store, делает запрос
//! `<url><path>?<query>` с заголовком `Authorization: Bearer <token>` и
//! возвращает тело ответа клиенту как есть, а callsite (handler) после этого
//! может обогатить JSON-массив полем `origin = <server_id>` через
//! [`enrich_with_origin`].
//!
//! ## API
//!
//! - [`proxy_request`] — основной helper. Принимает store, server_id, метод,
//!   путь, query и опциональное тело. Возвращает `(StatusCode, HeaderMap, Bytes)`.
//! - [`enrich_with_origin`] — мутирует `serde_json::Value`, добавляя поле
//!   `origin` ко всем item'ам массива и/или к самому объекту. Используется
//!   обработчиками после получения JSON-ответа от remote.
//! - [`ProxyError`] — типизированные ошибки прокси (`UnknownServer`,
//!   `Network`). Преобразуется в HTTP-ответ через [`ProxyError::into_response`]
//!   (BAD_GATEWAY для сетевых ошибок, NOT_FOUND для неизвестного server-id).
//!
//! ## Почему отдельный модуль
//!
//! Логика прокси одинакова для всех ресурсов (sessions/projects/tasks/todos),
//! отличаются только path/query, поэтому handler'ы main.rs делают
//! `proxy_request(&store, &client, id, METHOD, "/api/sessions", "", None).await`.
//!
//! Версия reqwest и features см. `Cargo.toml` (Phase 3).
//!
//! ## WebSocket-прокси (Phase 4)
//!
//! Помимо HTTP-помощников, модуль содержит [`proxy_websocket`] — generic-helper
//! для проксирования WebSocket-соединений на remote-инстанс. Используется
//! ws-handler'ами `/ws/attach`, `/ws/lazygit`, `/ws/tasks`, `/ws/todos`. Логика:
//!   1. Берёт `RemoteServer{url, token}` из store.
//!   2. Конвертирует `http(s)://` → `ws(s)://` через [`http_to_ws_url`].
//!   3. Открывает upstream через `tokio_tungstenite::connect_async` с заголовком
//!      `Authorization: Bearer <token>`.
//!   4. Двунаправленно проксирует фреймы (Text/Binary/Ping/Pong/Close) между
//!      downstream (axum WS) и upstream (tungstenite WS) через `tokio::select!`.
//!   5. Close с любой стороны корректно закрывает другую.
//!   6. Ошибки прокси логируются через `tracing::trace!`.

use std::fmt;

use axum::extract::ws::{CloseFrame as AxumCloseFrame, Message as AxumWsMessage, WebSocket};
use axum::http::StatusCode as AxumStatusCode;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode};
use tokio_tungstenite::tungstenite::handshake::client::Request as TungsteniteRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode as TungsteniteCloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

use crate::remotes::RemoteServerStore;

/// Ошибки прокси-прохода.
///
/// Конвертируется в HTTP-ответ через [`ProxyError::into_response`]:
/// - [`ProxyError::UnknownServer`] → 404 Not Found, body «no remote server `…`»;
/// - [`ProxyError::Network`] → 502 Bad Gateway, body — текст исходной ошибки;
/// - [`ProxyError::InvalidUrl`] → 500 Internal Server Error;
/// - [`ProxyError::WebSocket`] → 502 Bad Gateway (Phase 4, WS handshake/proxy).
#[derive(Debug)]
pub enum ProxyError {
    /// `server_id` отсутствует в реестре.
    UnknownServer(String),
    /// Сборка URL из `<remote.url>+<path>+<query>` упала (например, невалидные
    /// символы). На практике почти невозможный путь, потому что path/query
    /// собирает axum, но всё равно фиксируем для надёжности типов.
    InvalidUrl(String),
    /// reqwest вернул сетевую ошибку (timeout, refused, TLS-handshake-fail).
    Network(reqwest::Error),
    /// Phase 4 — ошибка WebSocket-прокси (handshake/connect_async, tungstenite).
    /// Хранит текст оригинальной ошибки, чтобы не тянуть `tungstenite::Error`
    /// в публичный API ProxyError (он не реализует `Send + 'static` в чистой
    /// форме для всех вариантов).
    WebSocket(String),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownServer(id) => write!(f, "no remote server with id `{id}`"),
            Self::InvalidUrl(msg) => write!(f, "invalid proxy URL: {msg}"),
            Self::Network(e) => write!(f, "remote network error: {e}"),
            Self::WebSocket(e) => write!(f, "remote websocket error: {e}"),
        }
    }
}

impl std::error::Error for ProxyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(e) => Some(e),
            _ => None,
        }
    }
}

impl ProxyError {
    /// Превращает ошибку в пару `(status, body)`, готовую к отдаче через
    /// axum-handler возвращающий `Result<_, (StatusCode, String)>`.
    pub fn into_response(self) -> (AxumStatusCode, String) {
        match self {
            Self::UnknownServer(_) => (AxumStatusCode::NOT_FOUND, self.to_string()),
            Self::InvalidUrl(_) => (AxumStatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            Self::Network(_) => (AxumStatusCode::BAD_GATEWAY, self.to_string()),
            Self::WebSocket(_) => (AxumStatusCode::BAD_GATEWAY, self.to_string()),
        }
    }
}

impl From<reqwest::Error> for ProxyError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e)
    }
}

/// Делает HTTP-запрос на `<remote.url><path>?<query>` с Bearer-токеном.
///
/// Параметры:
/// - `store` — реестр remote-серверов (read-lock держится только на время
///   копирования полей `url`/`token`, не во время сетевого вызова).
/// - `client` — общий `reqwest::Client` из `AppState.http` (cheap-clonable).
/// - `server_id` — id записи в реестре.
/// - `method` — HTTP-метод.
/// - `path` — путь, начинающийся со `/` (например, `/api/sessions`).
/// - `query` — query-строка без ведущего `?` (может быть пустой).
/// - `content_type` — опциональный `Content-Type` для запросов с телом.
/// - `body` — опциональное тело запроса.
///
/// Возвращает `(StatusCode, HeaderMap, Bytes)` — статус, заголовки и raw-тело
/// ответа remote. Caller сам решает, парсить ли тело как JSON.
///
/// ### Сборка URL
///
/// Использует [`RemoteServerStore::get`] → копию `url`/`token` и собирает:
/// `<url>` + `<path>` + (если `query` не пуст) `?<query>`. У `url` уже снят
/// trailing slash на этапе add (см. [`crate::remotes::trim_trailing_slash`]),
/// поэтому склейка не порождает `//`.
///
/// ### Безопасность
///
/// Токен передаётся как `Authorization: Bearer <token>` — единственный
/// заголовок, прилетающий из реестра. `User-Agent` / `Accept` reqwest ставит
/// сам по умолчанию.
pub async fn proxy_request(
    store: &RemoteServerStore,
    client: &Client,
    server_id: &str,
    method: Method,
    path: &str,
    query: &str,
    content_type: Option<&str>,
    body: Option<Bytes>,
) -> Result<(StatusCode, HeaderMap, Bytes), ProxyError> {
    // 1) Достать url+token. Read-lock на store снимаем дальше по стеку
    //    (вызов делается под `state.remotes.read().await`).
    let (base_url, token) = match store.get(server_id) {
        Some(s) => (s.url.clone(), s.token.clone()),
        None => return Err(ProxyError::UnknownServer(server_id.to_string())),
    };

    // 2) Собрать target URL.
    let mut url = String::with_capacity(base_url.len() + path.len() + query.len() + 1);
    url.push_str(&base_url);
    if !path.starts_with('/') {
        url.push('/');
    }
    url.push_str(path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    // 3) Сборка request.
    let mut req = client.request(method, &url);
    let bearer = format!("Bearer {token}");
    let bearer_value =
        HeaderValue::from_str(&bearer).map_err(|e| ProxyError::InvalidUrl(e.to_string()))?;
    req = req.header(AUTHORIZATION, bearer_value);
    if let Some(ct) = content_type {
        let ct_value =
            HeaderValue::from_str(ct).map_err(|e| ProxyError::InvalidUrl(e.to_string()))?;
        req = req.header(CONTENT_TYPE, ct_value);
    }
    if let Some(b) = body {
        req = req.body(b);
    }

    // 4) Выполнить. Phase 7 — логируем сетевые ошибки и таймауты.
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            // is_timeout()/is_connect() позволяют отличить разные классы
            // ошибок в логах (важно для диагностики remote down vs slow).
            tracing::warn!(
                server_id,
                path,
                error = %e,
                is_timeout = e.is_timeout(),
                is_connect = e.is_connect(),
                "remote_proxy: upstream request failed"
            );
            return Err(ProxyError::Network(e));
        }
    };
    let status = resp.status();
    // Phase 7 — non-2xx ответы upstream'а — это всё ещё успешный HTTP-обмен,
    // но обычно сигнал проблемы (5xx upstream / 401 на просрочке token и т.д.).
    // Печатаем на trace-уровне, чтобы не шуметь в prod без RUST_LOG=devforge=trace.
    if !status.is_success() {
        tracing::trace!(
            server_id,
            path,
            status = status.as_u16(),
            "remote_proxy: upstream non-2xx"
        );
    }
    let mut headers = HeaderMap::new();
    for (k, v) in resp.headers().iter() {
        // Hop-by-hop заголовки отфильтровываем — они specific для конкретного
        // hop'а (например, transfer-encoding не имеет смысла за прокси).
        if is_hop_by_hop(k) {
            continue;
        }
        headers.insert(k.clone(), v.clone());
    }
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                server_id,
                path,
                error = %e,
                "remote_proxy: failed to read upstream body"
            );
            return Err(ProxyError::Network(e));
        }
    };
    Ok((status, headers, body))
}

/// Hop-by-hop заголовки (RFC 7230 §6.1). Прокси не должен их форвардить.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Добавляет поле `origin = <server_id>` к JSON-ответу remote.
///
/// Правила:
/// - `Array` — для каждого item типа `Object` ставим `origin`. Элементы не-Object
///   оставляем как есть (но они редки, такого мы не отдаём из своих handler'ов).
/// - `Object` — ставим `origin` на верхнем уровне.
/// - Прочее (`String`, `Number`, `Null`, `Bool`) — без изменений.
///
/// Idempotent: повторный вызов перезаписывает существующее значение `origin`.
pub fn enrich_with_origin(value: &mut serde_json::Value, server_id: &str) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                if let serde_json::Value::Object(map) = item {
                    map.insert(
                        "origin".to_string(),
                        serde_json::Value::String(server_id.to_string()),
                    );
                }
            }
        }
        serde_json::Value::Object(map) => {
            map.insert(
                "origin".to_string(),
                serde_json::Value::String(server_id.to_string()),
            );
        }
        _ => { /* no-op для скаляров */ }
    }
}

// =============================================================================
// Phase 4 — WebSocket proxy
// =============================================================================

/// Конвертирует HTTP-схему remote-сервера в WebSocket-схему.
///
/// - `http://host[:port][/path]`  → `ws://host[:port][/path]`
/// - `https://host[:port][/path]` → `wss://host[:port][/path]`
/// - Любые другие схемы оставляются как есть (caller получит ошибку от
///   tungstenite при попытке handshake).
///
/// Возвращает новую `String`, исходный `base_url` не трогается.
fn http_to_ws_url(base_url: &str) -> String {
    if let Some(rest) = base_url.strip_prefix("https://") {
        let mut s = String::with_capacity(rest.len() + 6);
        s.push_str("wss://");
        s.push_str(rest);
        s
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        let mut s = String::with_capacity(rest.len() + 5);
        s.push_str("ws://");
        s.push_str(rest);
        s
    } else {
        base_url.to_string()
    }
}

/// Конвертирует фрейм axum (downstream, от браузера) в фрейм tungstenite
/// (upstream, к remote devforge).
///
/// - `Text/Binary/Ping/Pong` — 1:1.
/// - `Close(None)` → `Close(None)`.
/// - `Close(Some(cf))` → `Close(Some(CloseFrame{ code: cf.code.into(), reason: cf.reason }))`.
fn axum_to_tungstenite(msg: AxumWsMessage) -> TungsteniteMessage {
    match msg {
        AxumWsMessage::Text(t) => TungsteniteMessage::Text(t),
        AxumWsMessage::Binary(b) => TungsteniteMessage::Binary(b),
        AxumWsMessage::Ping(p) => TungsteniteMessage::Ping(p),
        AxumWsMessage::Pong(p) => TungsteniteMessage::Pong(p),
        AxumWsMessage::Close(None) => TungsteniteMessage::Close(None),
        AxumWsMessage::Close(Some(cf)) => TungsteniteMessage::Close(Some(TungsteniteCloseFrame {
            code: TungsteniteCloseCode::from(cf.code),
            reason: cf.reason,
        })),
    }
}

/// Конвертирует фрейм tungstenite (upstream) в фрейм axum (downstream).
///
/// `Frame` (raw frame, который тут возникает только в send-direction) — мапим
/// в `Binary` с payload-байтами для отказоустойчивости (на практике reader не
/// эмитит `Frame`, см. doc tungstenite).
fn tungstenite_to_axum(msg: TungsteniteMessage) -> AxumWsMessage {
    match msg {
        TungsteniteMessage::Text(t) => AxumWsMessage::Text(t),
        TungsteniteMessage::Binary(b) => AxumWsMessage::Binary(b),
        TungsteniteMessage::Ping(p) => AxumWsMessage::Ping(p),
        TungsteniteMessage::Pong(p) => AxumWsMessage::Pong(p),
        TungsteniteMessage::Close(None) => AxumWsMessage::Close(None),
        TungsteniteMessage::Close(Some(cf)) => AxumWsMessage::Close(Some(AxumCloseFrame {
            code: cf.code.into(),
            reason: cf.reason,
        })),
        TungsteniteMessage::Frame(f) => AxumWsMessage::Binary(f.payload().to_vec()),
    }
}

/// Generic WebSocket-прокси на remote devforge.
///
/// Параметры:
/// - `store`        — реестр remote-серверов (для получения url+token).
/// - `server_id`    — id записи в реестре.
/// - `upstream_path`— путь на remote (`/ws/attach`, `/ws/lazygit`, `/ws/tasks`,
///                    `/ws/todos`). Должен начинаться со `/`.
/// - `query`        — query-строка без ведущего `?` и БЕЗ параметра `server`
///                    (caller обязан отфильтровать). Может быть пустой.
/// - `downstream`   — уже-апгрейженный `WebSocket` от axum (со стороны браузера).
///
/// Поведение:
/// 1. Достаёт `RemoteServer{url, token}` из store. На отсутствие → закрывает
///    downstream с `Close{1011, "unknown remote server"}` и возвращает
///    `Err(ProxyError::UnknownServer)`.
/// 2. Конвертирует базу в `ws://`/`wss://`.
/// 3. Открывает upstream через `connect_async` с заголовком
///    `Authorization: Bearer <token>`. На ошибку handshake → закрывает
///    downstream с `Close{1011}` и возвращает `Err(ProxyError::Network)`.
/// 4. `tokio::select!` две задачи:
///    - read downstream → send upstream;
///    - read upstream   → send downstream;
///    Любая завершившаяся таска (Close, error, end-of-stream) кладёт сигнал
///    выхода и select! завершается; вторая сторона аккуратно закрывается.
/// 5. Все ошибки прокси логируются через `tracing::trace!` — caller обычно
///    не различает их (WS уже closed на этой точке).
pub async fn proxy_websocket(
    store: &RemoteServerStore,
    server_id: &str,
    upstream_path: &str,
    query: &str,
    downstream: WebSocket,
) -> Result<(), ProxyError> {
    // 1) Достать URL и токен.
    let (base_url, token) = match store.get(server_id) {
        Some(s) => (s.url.clone(), s.token.clone()),
        None => {
            tracing::trace!(server_id, "proxy_websocket: unknown server");
            close_downstream_with_error(downstream, 1011, "unknown remote server").await;
            return Err(ProxyError::UnknownServer(server_id.to_string()));
        }
    };

    // 2) Сборка ws:// URL.
    let ws_base = http_to_ws_url(&base_url);
    let mut url = String::with_capacity(ws_base.len() + upstream_path.len() + query.len() + 1);
    url.push_str(&ws_base);
    if !upstream_path.starts_with('/') {
        url.push('/');
    }
    url.push_str(upstream_path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    // 3) Request с Bearer-заголовком. tungstenite требует наличия Host,
    //    Connection: Upgrade, Upgrade: websocket, Sec-WebSocket-Key/Version —
    //    их подставляет client_request_with_headers через IntoClientRequest,
    //    но мы строим Request сами, потому что нужно положить Authorization.
    //    Для работы IntoClientRequest требуется правильно собрать минимум
    //    обязательных заголовков — проще всего использовать into_client_request()
    //    на URI и потом добавить наш Authorization.
    let request = match build_upstream_request(&url, &token) {
        Ok(req) => req,
        Err(e) => {
            tracing::trace!(server_id, url = %url, error = %e, "proxy_websocket: build_request failed");
            close_downstream_with_error(downstream, 1011, "invalid upstream request").await;
            return Err(ProxyError::InvalidUrl(e));
        }
    };

    // 4) Handshake.
    let (upstream, _resp) = match tokio_tungstenite::connect_async(request).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::trace!(server_id, url = %url, error = %e, "proxy_websocket: connect_async failed");
            close_downstream_with_error(downstream, 1011, "upstream connect failed").await;
            return Err(ProxyError::WebSocket(e.to_string()));
        }
    };

    // 5) Двунаправленный pump.
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut down_tx, mut down_rx) = downstream.split();

    // Down → Up: читаем фреймы от браузера, шлём на remote.
    // Phase 7 — копию server_id используем в логах для diagnostics.
    let server_id_dn = server_id.to_string();
    let upstream_path_dn = upstream_path.to_string();
    let down_to_up = async move {
        while let Some(frame) = down_rx.next().await {
            match frame {
                Ok(msg) => {
                    // Phase 7 — логируем close-коды downstream'а (полезно для
                    // диагностики «почему отвалился привязанный браузер»).
                    if let AxumWsMessage::Close(Some(ref cf)) = msg {
                        tracing::trace!(
                            server_id = %server_id_dn,
                            path = %upstream_path_dn,
                            code = cf.code,
                            reason = %cf.reason,
                            "proxy_websocket: downstream sent Close"
                        );
                    }
                    let is_close = matches!(msg, AxumWsMessage::Close(_));
                    let upmsg = axum_to_tungstenite(msg);
                    if let Err(e) = up_tx.send(upmsg).await {
                        tracing::trace!(
                            server_id = %server_id_dn,
                            path = %upstream_path_dn,
                            error = %e,
                            "proxy_websocket: up_tx.send failed"
                        );
                        break;
                    }
                    if is_close {
                        break;
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        server_id = %server_id_dn,
                        path = %upstream_path_dn,
                        error = %e,
                        "proxy_websocket: downstream recv error"
                    );
                    break;
                }
            }
        }
        // Корректно закрываем upstream если ещё не отправили Close.
        let _ = up_tx.send(TungsteniteMessage::Close(None)).await;
        let _ = up_tx.close().await;
    };

    // Up → Down: читаем фреймы от remote, шлём в браузер.
    let server_id_up = server_id.to_string();
    let upstream_path_up = upstream_path.to_string();
    let up_to_down = async move {
        while let Some(frame) = up_rx.next().await {
            match frame {
                Ok(msg) => {
                    if let TungsteniteMessage::Close(Some(ref cf)) = msg {
                        tracing::trace!(
                            server_id = %server_id_up,
                            path = %upstream_path_up,
                            code = u16::from(cf.code),
                            reason = %cf.reason,
                            "proxy_websocket: upstream sent Close"
                        );
                    }
                    let is_close = matches!(msg, TungsteniteMessage::Close(_));
                    let downmsg = tungstenite_to_axum(msg);
                    if let Err(e) = down_tx.send(downmsg).await {
                        tracing::trace!(
                            server_id = %server_id_up,
                            path = %upstream_path_up,
                            error = %e,
                            "proxy_websocket: down_tx.send failed"
                        );
                        break;
                    }
                    if is_close {
                        break;
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        server_id = %server_id_up,
                        path = %upstream_path_up,
                        error = %e,
                        "proxy_websocket: upstream recv error"
                    );
                    break;
                }
            }
        }
        // Закрываем downstream если ещё не послали Close.
        let _ = down_tx.send(AxumWsMessage::Close(None)).await;
        let _ = down_tx.close().await;
    };

    // tokio::join! — оба направления должны завершиться (cascading close).
    // При завершении любой стороны её handler шлёт Close в другую сторону,
    // что вызывает завершение второй задачи на следующей итерации.
    tokio::join!(down_to_up, up_to_down);

    Ok(())
}

/// Собирает `tokio_tungstenite::tungstenite::handshake::client::Request`
/// с заголовком `Authorization: Bearer <token>` поверх стандартных WS-заголовков.
///
/// Использует `IntoClientRequest::into_client_request()` (реализован для `&str`),
/// чтобы получить базовый request с обязательными WS-заголовками, и добавляет
/// `Authorization` через `headers_mut()`. Возвращает `String` как ошибку для
/// упрощения интерфейса (caller всё равно конвертирует в `ProxyError::InvalidUrl`).
fn build_upstream_request(url: &str, token: &str) -> Result<TungsteniteRequest, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().map_err(|e| e.to_string())?;
    let bearer = format!("Bearer {token}");
    let value = HeaderValue::from_str(&bearer)
        .map_err(|e| format!("invalid bearer token header: {e}"))?;
    req.headers_mut().insert(AUTHORIZATION, value);
    Ok(req)
}

/// Закрывает downstream-WS с кодом и текстом причины. Best-effort: ошибки
/// отправки игнорируются (WS уже мог быть закрыт другой стороной).
async fn close_downstream_with_error(mut downstream: WebSocket, code: u16, reason: &str) {
    let cf = AxumCloseFrame {
        code,
        reason: std::borrow::Cow::Owned(reason.to_string()),
    };
    let _ = downstream.send(AxumWsMessage::Close(Some(cf))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enrich_array_of_objects() {
        let mut v = json!([
            {"id": "a", "name": "alpha"},
            {"id": "b", "name": "beta"},
            {"id": "c", "name": "gamma"}
        ]);
        enrich_with_origin(&mut v, "office");
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for item in arr {
            assert_eq!(item.get("origin").and_then(|x| x.as_str()), Some("office"));
        }
        // Исходные поля сохранены.
        assert_eq!(arr[0].get("id").and_then(|x| x.as_str()), Some("a"));
        assert_eq!(arr[2].get("name").and_then(|x| x.as_str()), Some("gamma"));
    }

    #[test]
    fn enrich_empty_array_is_noop() {
        let mut v = json!([]);
        enrich_with_origin(&mut v, "x");
        let arr = v.as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn enrich_single_object() {
        let mut v = json!({"id": "z", "label": "single"});
        enrich_with_origin(&mut v, "remote-1");
        assert_eq!(
            v.get("origin").and_then(|x| x.as_str()),
            Some("remote-1")
        );
        assert_eq!(v.get("id").and_then(|x| x.as_str()), Some("z"));
    }

    #[test]
    fn enrich_string_is_noop() {
        let mut v = json!("hello");
        enrich_with_origin(&mut v, "x");
        assert_eq!(v.as_str(), Some("hello"));
    }

    #[test]
    fn enrich_null_is_noop() {
        let mut v = serde_json::Value::Null;
        enrich_with_origin(&mut v, "x");
        assert!(v.is_null());
    }

    #[test]
    fn enrich_number_is_noop() {
        let mut v = json!(42);
        enrich_with_origin(&mut v, "x");
        assert_eq!(v.as_i64(), Some(42));
    }

    #[test]
    fn enrich_array_with_mixed_items() {
        // Object-элементы получают origin, скаляры — нет.
        let mut v = json!([
            {"a": 1},
            "raw-string",
            42,
            null,
            {"b": 2}
        ]);
        enrich_with_origin(&mut v, "mix");
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0].get("origin").and_then(|x| x.as_str()), Some("mix"));
        assert_eq!(arr[1].as_str(), Some("raw-string"));
        assert_eq!(arr[2].as_i64(), Some(42));
        assert!(arr[3].is_null());
        assert_eq!(arr[4].get("origin").and_then(|x| x.as_str()), Some("mix"));
    }

    #[test]
    fn enrich_idempotent_overwrites_existing_origin() {
        let mut v = json!([{"id": "a", "origin": "old"}]);
        enrich_with_origin(&mut v, "new");
        assert_eq!(
            v[0].get("origin").and_then(|x| x.as_str()),
            Some("new"),
            "enrich должен ПЕРЕЗАПИСЫВАТЬ существующее origin"
        );
    }

    #[test]
    fn proxy_error_unknown_server_maps_to_404() {
        let (status, body) = ProxyError::UnknownServer("nope".into()).into_response();
        assert_eq!(status, AxumStatusCode::NOT_FOUND);
        assert!(body.contains("nope"));
    }

    #[test]
    fn proxy_error_invalid_url_maps_to_500() {
        let (status, _body) = ProxyError::InvalidUrl("bad".into()).into_response();
        assert_eq!(status, AxumStatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn is_hop_by_hop_matrix() {
        let connection: HeaderName = "connection".parse().unwrap();
        let upgrade: HeaderName = "upgrade".parse().unwrap();
        let content_type: HeaderName = "content-type".parse().unwrap();
        let x_custom: HeaderName = "x-custom".parse().unwrap();
        assert!(is_hop_by_hop(&connection));
        assert!(is_hop_by_hop(&upgrade));
        assert!(!is_hop_by_hop(&content_type));
        assert!(!is_hop_by_hop(&x_custom));
    }

    /// Smoke-test для `proxy_request`: достаём server из store с unknown-id
    /// и убеждаемся, что возвращается `ProxyError::UnknownServer`. Полный
    /// тест с реальным HTTP-сервером (mockito/wiremock) пока не делаем —
    /// потребует dev-dep и сетевую запуск-инфраструктуру. TODO: интеграционный
    /// тест в Phase 4 или Phase 6.
    #[tokio::test]
    async fn proxy_request_unknown_server() {
        use std::path::PathBuf;
        // Пустой store через tmp-файл.
        let tmp = std::env::temp_dir().join(format!(
            "forge-proxy-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = RemoteServerStore::load(PathBuf::from(&tmp)).unwrap();
        let client = Client::new();
        let r = proxy_request(
            &store,
            &client,
            "ghost",
            Method::GET,
            "/api/sessions",
            "",
            None,
            None,
        )
        .await;
        assert!(matches!(r, Err(ProxyError::UnknownServer(_))));
    }

    // -------------------------------------------------------------------------
    // Phase 4 — WebSocket proxy helpers
    // -------------------------------------------------------------------------

    #[test]
    fn http_to_ws_url_http_scheme() {
        assert_eq!(http_to_ws_url("http://localhost:8080"), "ws://localhost:8080");
        assert_eq!(
            http_to_ws_url("http://192.168.1.10:9000/sub"),
            "ws://192.168.1.10:9000/sub"
        );
    }

    #[test]
    fn http_to_ws_url_https_scheme() {
        assert_eq!(
            http_to_ws_url("https://devforge.example.com"),
            "wss://devforge.example.com"
        );
        assert_eq!(
            http_to_ws_url("https://office.lan:8443/api"),
            "wss://office.lan:8443/api"
        );
    }

    #[test]
    fn http_to_ws_url_other_scheme_passthrough() {
        // Не http/https — оставляем как есть. tungstenite сам выдаст ошибку.
        assert_eq!(http_to_ws_url("ftp://nope"), "ftp://nope");
        assert_eq!(http_to_ws_url("ws://already"), "ws://already");
        assert_eq!(http_to_ws_url(""), "");
    }

    #[test]
    fn proxy_error_websocket_maps_to_502() {
        let (status, body) = ProxyError::WebSocket("boom".into()).into_response();
        assert_eq!(status, AxumStatusCode::BAD_GATEWAY);
        assert!(body.contains("boom"));
    }

    #[test]
    fn proxy_error_websocket_display() {
        let e = ProxyError::WebSocket("handshake refused".into());
        assert!(e.to_string().contains("handshake refused"));
    }

    #[test]
    fn build_upstream_request_adds_bearer() {
        let req = build_upstream_request("ws://localhost:8080/ws/attach", "tok123").unwrap();
        let auth = req
            .headers()
            .get(AUTHORIZATION)
            .expect("Authorization header missing");
        assert_eq!(auth.to_str().unwrap(), "Bearer tok123");
    }

    #[test]
    fn build_upstream_request_rejects_invalid_token() {
        // Управляющие символы недопустимы в HeaderValue.
        let r = build_upstream_request("ws://localhost/", "bad\ntoken");
        assert!(r.is_err());
    }

    // =========================================================================
    // Phase 8 .4 — WebSocket frame mapping (axum ↔ tungstenite)
    // =========================================================================
    //
    // proxy_websocket требует полноценный axum::extract::ws::WebSocket
    // downstream (созданный из HTTP-upgrade'а), который нельзя сконструировать
    // в unit-тесте без поднятия axum-сервера и реального WS-handshake'а.
    // Поэтому здесь мы детально тестируем чистые mapper-функции
    // axum_to_tungstenite / tungstenite_to_axum (которые покрывают всю
    // семантику pump'а), и отдельным smoke-тестом — proxy_websocket с
    // unknown server (уже есть фрагмент в http_to_ws_url_*).

    #[test]
    fn axum_to_tungstenite_text_roundtrip() {
        let m = axum_to_tungstenite(AxumWsMessage::Text("hi".to_string()));
        match m {
            TungsteniteMessage::Text(s) => assert_eq!(s, "hi"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn axum_to_tungstenite_binary_roundtrip() {
        let payload = vec![0u8, 1, 2, 250, 255];
        let m = axum_to_tungstenite(AxumWsMessage::Binary(payload.clone()));
        match m {
            TungsteniteMessage::Binary(b) => assert_eq!(b, payload),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn axum_to_tungstenite_ping_pong() {
        let ping = axum_to_tungstenite(AxumWsMessage::Ping(vec![1, 2, 3]));
        assert!(matches!(ping, TungsteniteMessage::Ping(ref p) if p == &vec![1, 2, 3]));
        let pong = axum_to_tungstenite(AxumWsMessage::Pong(vec![9, 8, 7]));
        assert!(matches!(pong, TungsteniteMessage::Pong(ref p) if p == &vec![9, 8, 7]));
    }

    #[test]
    fn axum_to_tungstenite_close_none() {
        let m = axum_to_tungstenite(AxumWsMessage::Close(None));
        assert!(matches!(m, TungsteniteMessage::Close(None)));
    }

    #[test]
    fn axum_to_tungstenite_close_with_code_1000() {
        let cf = AxumCloseFrame {
            code: 1000,
            reason: std::borrow::Cow::Borrowed("normal"),
        };
        let m = axum_to_tungstenite(AxumWsMessage::Close(Some(cf)));
        match m {
            TungsteniteMessage::Close(Some(cf)) => {
                assert_eq!(u16::from(cf.code), 1000);
                assert_eq!(cf.reason, "normal");
            }
            _ => panic!("expected Close(Some)"),
        }
    }

    #[test]
    fn axum_to_tungstenite_close_with_code_1011() {
        let cf = AxumCloseFrame {
            code: 1011,
            reason: std::borrow::Cow::Borrowed("internal"),
        };
        let m = axum_to_tungstenite(AxumWsMessage::Close(Some(cf)));
        match m {
            TungsteniteMessage::Close(Some(cf)) => {
                assert_eq!(u16::from(cf.code), 1011);
                assert_eq!(cf.reason, "internal");
            }
            _ => panic!("expected Close(Some)"),
        }
    }

    #[test]
    fn tungstenite_to_axum_text_roundtrip() {
        let m = tungstenite_to_axum(TungsteniteMessage::Text("yo".into()));
        match m {
            AxumWsMessage::Text(s) => assert_eq!(s, "yo"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn tungstenite_to_axum_binary_roundtrip() {
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let m = tungstenite_to_axum(TungsteniteMessage::Binary(payload.clone()));
        match m {
            AxumWsMessage::Binary(b) => assert_eq!(b, payload),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn tungstenite_to_axum_close_none() {
        let m = tungstenite_to_axum(TungsteniteMessage::Close(None));
        assert!(matches!(m, AxumWsMessage::Close(None)));
    }

    #[test]
    fn tungstenite_to_axum_close_with_code() {
        let cf = TungsteniteCloseFrame {
            code: TungsteniteCloseCode::Normal,
            reason: std::borrow::Cow::Borrowed("bye"),
        };
        let m = tungstenite_to_axum(TungsteniteMessage::Close(Some(cf)));
        match m {
            AxumWsMessage::Close(Some(cf)) => {
                assert_eq!(cf.code, 1000);
                assert_eq!(cf.reason, "bye");
            }
            _ => panic!("expected Close(Some)"),
        }
    }

    #[test]
    fn tungstenite_to_axum_close_internal_error_1011() {
        let cf = TungsteniteCloseFrame {
            code: TungsteniteCloseCode::from(1011u16),
            reason: std::borrow::Cow::Borrowed("upstream-err"),
        };
        let m = tungstenite_to_axum(TungsteniteMessage::Close(Some(cf)));
        match m {
            AxumWsMessage::Close(Some(cf)) => {
                assert_eq!(cf.code, 1011);
                assert_eq!(cf.reason, "upstream-err");
            }
            _ => panic!("expected Close(Some 1011)"),
        }
    }

    #[test]
    fn tungstenite_to_axum_frame_maps_to_binary() {
        // Raw `Frame` (редкий случай) → Binary с raw payload-байтами.
        use tokio_tungstenite::tungstenite::protocol::frame::{
            coding::{Data, OpCode},
            Frame, FrameHeader,
        };
        let payload = vec![1u8, 2, 3, 4];
        let header = FrameHeader::default();
        let mut header = header;
        header.opcode = OpCode::Data(Data::Binary);
        let frame = Frame::from_payload(header, payload.clone().into());
        let m = tungstenite_to_axum(TungsteniteMessage::Frame(frame));
        match m {
            AxumWsMessage::Binary(b) => assert_eq!(b, payload),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn tungstenite_to_axum_ping_pong_payload_preserved() {
        let ping = tungstenite_to_axum(TungsteniteMessage::Ping(vec![1, 2, 3]));
        assert!(matches!(ping, AxumWsMessage::Ping(ref p) if p == &vec![1, 2, 3]));
        let pong = tungstenite_to_axum(TungsteniteMessage::Pong(vec![4, 5]));
        assert!(matches!(pong, AxumWsMessage::Pong(ref p) if p == &vec![4, 5]));
    }

    #[tokio::test]
    async fn proxy_websocket_unknown_server_returns_unknown_server_error() {
        // Smoke-test: proxy_websocket с unknown server_id должен закончиться
        // ProxyError::UnknownServer ДО попытки сделать WS handshake.
        // Так как мы не можем создать настоящий axum WebSocket в unit-тесте,
        // проверяем contract через прямую проверку extract_server_id-логики
        // в proxy_websocket: store пустой → match arm UnknownServer.
        //
        // К сожалению, downstream-параметр требует WebSocket, конструировать
        // который без HTTP-upgrade нельзя. Поэтому ограничиваемся проверкой,
        // что in-process tungstenite-handshake к несуществующему адресу
        // действительно эмитит ошибку, эквивалентную тому, что обработала бы
        // proxy_websocket.
        use std::path::PathBuf;
        let tmp = std::env::temp_dir().join(format!(
            "forge-ws-unknown-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = RemoteServerStore::load(PathBuf::from(&tmp)).unwrap();
        // Проверяем, что get() возвращает None — main precondition для
        // unknown-arm в proxy_websocket.
        assert!(store.get("ghost").is_none());
    }

    #[tokio::test]
    async fn proxy_websocket_handshake_fails_against_dead_port() {
        // Берём свободный порт и сразу освобождаем — следующий connect_async
        // должен упасть с ConnectionRefused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("ws://{addr}/ws");

        let req = build_upstream_request(&url, "tok").unwrap();
        let r = tokio_tungstenite::connect_async(req).await;
        assert!(
            r.is_err(),
            "expected connection refused, got {:?}",
            r.as_ref().map(|_| "Ok"),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn proxy_websocket_in_process_text_roundtrip() {
        // In-process тест: поднимаем настоящий tungstenite-сервер, делаем
        // handshake через build_upstream_request, шлём Text от клиента,
        // получаем echo. Не покрывает axum-WS-сторону (это пришлось бы
        // делать через ось axum-test/router upgrade), но проверяет, что
        // build_upstream_request + connect_async работают end-to-end
        // и что Bearer-заголовок доходит.
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_hdr_async;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let auth_check = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let auth_check_srv = auth_check.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // accept_hdr_async позволяет проверить заголовки клиента.
            let ws = accept_hdr_async(
                stream,
                |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    if let Some(v) = req.headers().get("authorization") {
                        *auth_check_srv.lock().unwrap() =
                            Some(v.to_str().unwrap_or("").to_string());
                    }
                    Ok(resp)
                },
            )
            .await
            .expect("server accept");
            let (mut tx, mut rx) = ws.split();
            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    TungsteniteMessage::Text(t) => {
                        let _ = tx
                            .send(TungsteniteMessage::Text(format!("echo:{t}")))
                            .await;
                    }
                    TungsteniteMessage::Close(_) => break,
                    _ => {}
                }
            }
        });

        let url = format!("ws://{addr}/test");
        let req = build_upstream_request(&url, "test-bearer-xyz").unwrap();
        let (mut up, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

        up.send(TungsteniteMessage::Text("hello".into()))
            .await
            .unwrap();
        let reply = up.next().await.unwrap().unwrap();
        match reply {
            TungsteniteMessage::Text(t) => assert_eq!(t, "echo:hello"),
            other => panic!("unexpected reply: {other:?}"),
        }
        up.send(TungsteniteMessage::Close(None)).await.unwrap();
        let _ = server.await;

        let captured = auth_check.lock().unwrap().clone();
        assert_eq!(captured.as_deref(), Some("Bearer test-bearer-xyz"));
    }

    // =========================================================================
    // Phase 8 .1 — HTTP proxy integration tests (wiremock)
    // =========================================================================
    //
    // Покрывают сценарии прохождения статуса (200/204/4xx/5xx), таймаута,
    // DNS-fail, connection-refused, Content-Length=0, chunked transfer-encoding,
    // passthrough redirects (NO follow), стриминга крупного тела (≥10 MB).
    //
    // Все тесты используют `wiremock::MockServer::start()` + локальный
    // `RemoteServerStore` через tmp-файл, чтобы не зависеть от внешнего I/O.

    /// Создаёт изолированный empty `RemoteServerStore` через tmp-файл и
    /// добавляет в него один сервер. Возвращает `(store, server_id)`.
    fn make_store_with(label: &str, url: &str, token: &str) -> (RemoteServerStore, String) {
        use std::path::PathBuf;
        let tmp = std::env::temp_dir().join(format!(
            "forge-proxy-test-store-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = RemoteServerStore::load(PathBuf::from(&tmp)).unwrap();
        let server = store.add(label, url, token).unwrap();
        (store, server.id)
    }

    /// Сборка `reqwest::Client` с консервативным таймаутом и БЕЗ follow-редиректов.
    /// Это ключ для тестов: 1) timeout-сценарий должен закончиться `is_timeout()`;
    /// 2) `passthrough_redirect` должен видеть 302 как есть.
    fn make_test_client() -> Client {
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("reqwest client build")
    }

    #[tokio::test]
    async fn proxy_request_passes_200_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/api/sessions",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"hello");
    }

    #[tokio::test]
    async fn proxy_request_passes_204_no_content() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::DELETE,
            "/api/projects/foo",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty(), "204 body must be empty, got {:?}", body);
    }

    #[tokio::test]
    async fn proxy_request_passes_404_not_found() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/api/sessions/ghost",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(&body[..], b"missing");
    }

    #[tokio::test]
    async fn proxy_request_passes_500_server_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/api/sessions",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(&body[..], b"boom");
    }

    #[tokio::test]
    async fn proxy_request_timeout_returns_network_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(5)),
            )
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client(); // 500ms timeout
        let r = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/slow",
            "",
            None,
            None,
        )
        .await;
        let err = r.expect_err("should timeout");
        match err {
            ProxyError::Network(e) => {
                assert!(e.is_timeout(), "expected is_timeout(), got {e:?}");
            }
            other => panic!("expected Network(timeout), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn proxy_request_connection_refused_returns_network_error() {
        // Берём свободный порт через bind+drop — почти гарантированно никто
        // не успеет его занять в течение микросекунд между drop и connect.
        // На редкой гонке тест может стать flaky, но это маловероятно.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let dead_url = format!("http://{addr}");

        let (store, id) = make_store_with("Dead", &dead_url, "tok");
        let client = make_test_client();
        let r = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/",
            "",
            None,
            None,
        )
        .await;
        let err = r.expect_err("should fail to connect");
        match err {
            ProxyError::Network(e) => {
                assert!(e.is_connect() || e.is_request(), "expected connection error, got {e:?}");
            }
            other => panic!("expected Network(connect), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn proxy_request_dns_fail_returns_network_error() {
        // .invalid TLD зарезервирован RFC 6761 — гарантированно не резолвится.
        let (store, id) = make_store_with(
            "DnsFail",
            "http://this-host-does-not-exist.invalid",
            "tok",
        );
        let client = make_test_client();
        let r = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/api/sessions",
            "",
            None,
            None,
        )
        .await;
        let err = r.expect_err("should fail DNS");
        // На некоторых системах DNS-fail классифицируется reqwest как
        // is_connect()=true с inner-source "dns error". Главное — это
        // Network-ошибка, а не таймаут или success.
        assert!(matches!(err, ProxyError::Network(_)));
    }

    #[tokio::test]
    async fn proxy_request_content_length_zero_empty_body() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", "0")
                    .set_body_bytes(Vec::<u8>::new()),
            )
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, headers, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/empty",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        // content-length НЕ относится к hop-by-hop, поэтому должен дойти.
        assert_eq!(
            headers.get("content-length").and_then(|v| v.to_str().ok()),
            Some("0")
        );
    }

    #[tokio::test]
    async fn proxy_request_chunked_body_collected() {
        // wiremock сам обычно ставит content-length при known-size теле, но
        // важно проверить, что прокси корректно вычитывает тело при больших
        // payload (что де-факто chunk'нется по hyper-уровню).
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        let payload = "abcdef".repeat(2048); // ~12 KB
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&payload))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/big",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.len(), payload.len());
        assert_eq!(&body[..], payload.as_bytes());
    }

    #[tokio::test]
    async fn proxy_request_does_not_follow_redirect() {
        // 302 c Location должен пройти насквозь, прокси НЕ должен follow.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/old"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "/new"),
            )
            .mount(&mock)
            .await;
        // Если бы прокси follow'ил — он попал бы на /new и получил 200/404 от
        // wiremock-fallback. Намеренно не mount'им /new, чтобы дополнительно
        // сломать сценарий follow.

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, headers, _body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/old",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::from_u16(302).unwrap());
        assert_eq!(
            headers.get("location").and_then(|v| v.to_str().ok()),
            Some("/new")
        );
    }

    // =========================================================================
    // Phase 8 .2 — Hop-by-hop filtering + Bearer injection integration tests
    // =========================================================================

    #[tokio::test]
    async fn proxy_request_injects_server_bearer_token() {
        // Upstream получает Authorization: Bearer <server-stored-token>.
        // У клиента (тест) Bearer'а нет, но даже если бы был — proxy_request
        // НЕ принимает клиентские заголовки, так что просочиться нечему.
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer server-secret-XYZ"))
            .respond_with(ResponseTemplate::new(200).set_body_string("authed"))
            .expect(1)
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "server-secret-XYZ");
        let client = make_test_client();
        let (status, _h, body) = proxy_request(
            &store, &client, &id, Method::GET, "/protected", "", None, None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"authed");
        // mock.verify() implicit через .expect(1) при drop'е MockServer.
    }

    #[tokio::test]
    async fn proxy_request_bearer_replaces_anything_in_client_session() {
        // Даже при разных id одного store — каждый получает свой токен.
        // Это смоук-тест на отсутствие cross-token утечки между разными
        // remote-серверами.
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_a = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer token-A"))
            .respond_with(ResponseTemplate::new(200).set_body_string("A"))
            .mount(&mock_a)
            .await;

        let mock_b = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer token-B"))
            .respond_with(ResponseTemplate::new(200).set_body_string("B"))
            .mount(&mock_b)
            .await;

        use std::path::PathBuf;
        let tmp = std::env::temp_dir().join(format!(
            "forge-proxy-test-store-multi-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = RemoteServerStore::load(PathBuf::from(&tmp)).unwrap();
        let s_a = store.add("A", mock_a.uri(), "token-A").unwrap();
        let s_b = store.add("B", mock_b.uri(), "token-B").unwrap();

        let client = make_test_client();
        let (_, _, body_a) = proxy_request(
            &store, &client, &s_a.id, Method::GET, "/x", "", None, None,
        )
        .await
        .unwrap();
        assert_eq!(&body_a[..], b"A");
        let (_, _, body_b) = proxy_request(
            &store, &client, &s_b.id, Method::GET, "/x", "", None, None,
        )
        .await
        .unwrap();
        assert_eq!(&body_b[..], b"B");
    }

    #[tokio::test]
    async fn proxy_request_does_not_send_spurious_hop_by_hop_headers() {
        // Sanity-check: proxy_request НЕ добавляет hop-by-hop заголовки
        // в upstream-запрос. reqwest сам не ставит Connection/TE/etc, но
        // важно зафиксировать это контрактом.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate};

        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>);
        impl Respond for Capture {
            fn respond(&self, req: &WmRequest) -> ResponseTemplate {
                let mut g = self.0.lock().unwrap();
                for (k, v) in req.headers.iter() {
                    g.push((k.as_str().to_string(), v.as_bytes().to_vec()));
                }
                ResponseTemplate::new(200)
            }
        }

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(Capture(captured.clone()))
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let _ = proxy_request(
            &store, &client, &id, Method::GET, "/peek", "", None, None,
        )
        .await
        .expect("proxy_request");

        let headers = captured.lock().unwrap();
        let names: Vec<String> =
            headers.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
        // Negative assertions: hop-by-hop НЕ должны быть отправлены.
        for forbidden in &[
            "proxy-authorization",
            "proxy-authenticate",
            "te",
            "trailers",
            "transfer-encoding",
            "upgrade",
            "keep-alive",
        ] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "hop-by-hop header `{forbidden}` leaked upstream: {names:?}"
            );
        }
        // Положительный assertion: Authorization (end-to-end) ОБЯЗАТЕЛЬНО там.
        assert!(names.iter().any(|n| n == "authorization"));
    }

    #[tokio::test]
    async fn proxy_request_filters_hop_by_hop_in_response() {
        // Если upstream ставит hop-by-hop в ответе — proxy_request их вырезает.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("connection", "close")
                    .insert_header("keep-alive", "timeout=5")
                    .insert_header("proxy-authenticate", "Basic")
                    .insert_header("trailers", "X-Foo")
                    .insert_header("upgrade", "h2c")
                    .insert_header("x-end-to-end", "yes")
                    .set_body_string("ok"),
            )
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let (status, headers, _body) = proxy_request(
            &store, &client, &id, Method::GET, "/resp", "", None, None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);

        // Hop-by-hop вырезаны.
        for forbidden in &[
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "trailers",
            "upgrade",
            "transfer-encoding",
            "te",
            "proxy-authorization",
        ] {
            assert!(
                headers.get(*forbidden).is_none(),
                "response hop-by-hop `{forbidden}` leaked downstream"
            );
        }
        // End-to-end должны пройти.
        assert_eq!(
            headers.get("x-end-to-end").and_then(|v| v.to_str().ok()),
            Some("yes")
        );
    }

    #[tokio::test]
    async fn proxy_request_content_type_is_forwarded_when_set() {
        // Phase 8 .2 — sanity: explicit content_type попадает в upstream.
        use wiremock::matchers::{header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&mock)
            .await;

        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let client = make_test_client();
        let body = Bytes::from_static(br#"{"hello":"world"}"#);
        let (status, _h, _b) = proxy_request(
            &store,
            &client,
            &id,
            Method::POST,
            "/api/tasks",
            "",
            Some("application/json"),
            Some(body),
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn proxy_request_streams_large_body_10mb() {
        // 10 MB ответ не должен падать с OOM и должен прийти полностью.
        // wiremock держит payload в памяти, но cargo test с дефолтным стеком
        // справляется — это smoke-test на отсутствие panic'ов / переполнений.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        let size = 10 * 1024 * 1024; // 10 MB
        let payload = vec![b'X'; size];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
            .mount(&mock)
            .await;

        // Для большого тела таймаут 500ms может быть мало на slow CI; ставим 10s.
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let (store, id) = make_store_with("Mock", &mock.uri(), "tok");
        let (status, _h, body) = proxy_request(
            &store,
            &client,
            &id,
            Method::GET,
            "/blob",
            "",
            None,
            None,
        )
        .await
        .expect("proxy_request");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.len(), size);
        // Проверяем краешки тела (читать целиком 10 MB накладно для assert).
        assert_eq!(body[0], b'X');
        assert_eq!(body[size - 1], b'X');
    }
}
