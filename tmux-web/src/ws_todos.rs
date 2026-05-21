//! WebSocket-handler `/ws/todos` для realtime TODO-стрима.
//!
//! Клиент подключается → сервер отправляет полный `{kind:"snapshot",todos:[...]}`
//! (то же, что вернул бы `GET /api/todos?path=...`) → дальше шлёт
//! `{kind:"upsert",todo:...}` / `{kind:"removed",id:"..."}` / `{kind:"reload"}`
//! по мере мутаций через REST `/api/todos*`.
//!
//! ### Wire-протокол
//!
//! Все сообщения сервер→клиент — `Message::Text` с JSON-тэгом `kind`:
//!
//! - `{kind:"snapshot", todos: [...]}` — при connect или после reload-сигнала.
//! - `{kind:"upsert", todo: {...}}` — TODO создана / изменена.
//! - `{kind:"removed", id:"..."}` — TODO удалена (включая promote).
//! - `{kind:"reload"}` — клиенту следует сделать `fetchTodos()`
//!   (используется при переполнении broadcast-канала).
//!
//! ### Фильтрация по cwd (root_path)
//!
//! WS-handler принимает query-параметр `path` (cwd сессии). При connect
//! сервер делает `paths::resolve_root(path)` и сохраняет результат как
//! подписной `root_path`. События [`TodoEvent`] несут поле `root_path`;
//! handler форвардит клиенту только те, у которых `event.root_path()`
//! совпадает с подписным.
//!
//! Если `path` не передан — клиент получает поток без фильтра (admin/debug
//! режим): подписывается на ВСЕ события. Snapshot в этом режиме пустой
//! (нет «дефолтного» корня).
//!
//! Snapshot отдаётся по тому же фильтру, что и последующие события.
//!
//! ### Backpressure / lag
//!
//! `broadcast::Receiver::recv` возвращает `Err(RecvError::Lagged(n))` когда
//! sender обогнал receiver'а на `n` сообщений (buffer = 64). В этом случае
//! шлём клиенту `{kind:"reload"}` — он ресинхронизирует state через
//! `fetchTodos()`, и WS продолжает работу.
//!
//! ### Heartbeat
//!
//! Каждые 30s — Ping; axum обрабатывает Pong автоматически. Это позволяет
//! детектить полу-открытые соединения (NAT timeout, proxy idle).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;

use crate::remote_proxy;
use crate::todos::Todo;
use crate::AppState;

/// Heartbeat-период. 30s — стандартный компромисс: достаточно часто, чтобы
/// детектить полу-открытые соединения (NAT timeout, proxy idle), но не
/// настолько часто, чтобы нагружать CPU/сеть.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Событие TODO для broadcast-канала.
///
/// Сериализуется через `serde(tag = "kind", rename_all = "snake_case")` —
/// итоговый JSON совместим с фронтенд-протоколом `/ws/todos`:
/// `{"kind":"upsert","todo":{...}}`, `{"kind":"removed","root_path":"...","id":"..."}`,
/// `{"kind":"reload","root_path":"..."}`.
///
/// `Snapshot` — отдельный «синтетический» вариант, **не** идущий в broadcast:
/// формируется на стороне handler'а при connect и шлётся напрямую клиенту,
/// чтобы не плодить snapshot-флуд по всем подписчикам.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoEvent {
    /// TODO создана/обновлена. `root_path` достаётся из `todo.root_path`.
    Upsert { todo: Todo },
    /// TODO удалена. Несёт `root_path` (для фильтрации) и `id`.
    Removed { root_path: String, id: String },
    /// Сигнал клиентам: ресинхронизироваться через `fetchTodos()`.
    Reload { root_path: String },
}

impl TodoEvent {
    /// Возвращает `root_path`, к которому относится событие. Используется
    /// сервером для фильтрации broadcast-стрима по `root_path` подписчика.
    pub fn root_path(&self) -> &str {
        match self {
            TodoEvent::Upsert { todo } => &todo.root_path,
            TodoEvent::Removed { root_path, .. } => root_path,
            TodoEvent::Reload { root_path } => root_path,
        }
    }
}

/// Query-параметры WS-handler'а. `path` — cwd сессии. Если не задан, клиент
/// получает все события (admin/debug режим), но snapshot пустой.
///
/// Handler фактически использует `Query<HashMap<String,String>>` для поддержки
/// `?server=<id>`-прокси; struct оставлен как документация контракта query.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct TodoWsQuery {
    #[serde(default)]
    pub path: Option<String>,
}

/// `GET /ws/todos?path=<cwd>` — upgrade в WebSocket, далее [`handle_socket`].
///
/// Если `path` задан — сервер делает `paths::resolve_root(path)` и
/// форвардит клиенту только события с таким же `root_path`. Без `path` —
/// клиент получает все события (admin/debug режим) без фильтрации.
///
/// ### Поддержка `?server=<id>` (remote proxy)
///
/// При `?server=<id>` делегирует в [`remote_proxy::proxy_websocket`] на
/// upstream `/ws/todos` (с query без `server`). При `server` + `remote_mode=false`
/// → Close{1008}.
pub async fn todos_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response {
    // Remote-proxy ветка.
    if let Some(server_id) = extract_server_id(&raw) {
        if !state.remote_mode {
            tracing::warn!(server_id, "ws/todos: ?server requested in non-remote mode");
            return ws.on_upgrade(move |socket| close_with_policy_violation(socket, "remote mode disabled"));
        }
        let upstream_query = rebuild_query_without_server(&raw);
        return ws.on_upgrade(move |socket| async move {
            let store = state.remotes.read().await;
            if let Err(e) = remote_proxy::proxy_websocket(
                &store,
                &server_id,
                "/ws/todos",
                &upstream_query,
                socket,
            )
            .await
            {
                tracing::trace!(error = %e, server_id, "ws/todos proxy_websocket finished with error");
            }
        });
    }

    // cwd-based фильтр: если path задан — резолвим до root, иначе None (без фильтра).
    let subscribed_root: Option<String> = match raw.get("path").map(|s| s.as_str()) {
        Some(s) if !s.trim().is_empty() => {
            let root = crate::paths::resolve_root(std::path::Path::new(s.trim()));
            Some(root.to_string_lossy().to_string())
        }
        _ => None,
    };
    tracing::info!(root = ?subscribed_root, "ws/todos upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state, subscribed_root))
}

/// Основной обработчик одного WS-соединения.
///
/// `subscribed_root` = `Some(root)` — клиент получает только события этого
/// корня; `None` — все события (admin/debug режим), snapshot пустой.
async fn handle_socket(socket: WebSocket, state: AppState, subscribed_root: Option<String>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // 1) Snapshot текущего состояния TODO для подписного root (или [] если без фильтра).
    let todos = match subscribed_root.as_deref() {
        Some(root) => state.todos.list(root),
        None => Vec::new(),
    };
    let snapshot_msg = serde_json::json!({
        "kind": "snapshot",
        "todos": todos,
    });
    if let Err(e) = send_text(&ws_tx, snapshot_msg.to_string()).await {
        tracing::debug!(error = ?e, "ws/todos: snapshot send failed; closing");
        return;
    }

    // 2) Подписываемся на broadcast.
    let mut rx = state.todos_tx.subscribe();

    // 3) Heartbeat таймер.
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // Первый tick срабатывает сразу — пропускаем, чтобы не дублировать snapshot.
    heartbeat.tick().await;

    // 4) Главный select-loop.
    loop {
        tokio::select! {
            biased;

            // Broadcast: TodoEvent → JSON Text (с фильтром по root_path).
            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        if let Some(ref root) = subscribed_root {
                            if event.root_path() != root {
                                continue;
                            }
                        }
                        let json = match serde_json::to_string(&event) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!(error = ?e, "TodoEvent serialize failed");
                                continue;
                            }
                        };
                        if let Err(e) = send_text(&ws_tx, json).await {
                            tracing::debug!(error = ?e, "ws/todos: event send failed; closing");
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(lag = n, "ws/todos: subscriber lagged, sending reload");
                        let reload = serde_json::json!({"kind": "reload"}).to_string();
                        if let Err(e) = send_text(&ws_tx, reload).await {
                            tracing::debug!(error = ?e, "ws/todos: reload send failed; closing");
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        tracing::info!("ws/todos: broadcast closed");
                        break;
                    }
                }
            }

            // Heartbeat: Ping каждые 30s.
            _ = heartbeat.tick() => {
                let mut guard = ws_tx.lock().await;
                if let Err(e) = guard.send(Message::Ping(Vec::new())).await {
                    tracing::debug!(error = ?e, "ws/todos: ping failed; closing");
                    break;
                }
            }

            // Inbound: ожидаем Pong / Close, остальное — игнор.
            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!("ws/todos: client closed");
                        break;
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {
                        // axum обрабатывает Ping автоматически; Pong — игнор.
                    }
                    Some(Ok(other)) => {
                        tracing::debug!(?other, "ws/todos: unexpected inbound message; ignored");
                    }
                    Some(Err(e)) => {
                        tracing::debug!(error = ?e, "ws/todos: ws recv error");
                        break;
                    }
                }
            }
        }
    }

    // Best-effort close.
    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::info!("ws/todos session terminated");
}

/// Отправить Text-фрейм через shared sink. Возвращает Err при сетевом фейле.
async fn send_text(
    ws_tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    text: String,
) -> Result<(), axum::Error> {
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

/// Helper: сформировать `Upsert` событие из Todo.
pub fn upsert(todo: Todo) -> TodoEvent {
    TodoEvent::Upsert { todo }
}

/// Helper: сформировать `Removed` событие.
pub fn removed(root_path: impl Into<String>, id: impl Into<String>) -> TodoEvent {
    TodoEvent::Removed {
        root_path: root_path.into(),
        id: id.into(),
    }
}

/// Helper: сформировать `Reload` событие.
#[allow(dead_code)]
pub fn reload(root_path: impl Into<String>) -> TodoEvent {
    TodoEvent::Reload {
        root_path: root_path.into(),
    }
}

// =============================================================================
// Phase 4 — helpers для remote WS-proxy ветки
// =============================================================================

/// Извлекает значение `server` из query. Возвращает `Some` только при непустом
/// значении (после trim).
fn extract_server_id(q: &HashMap<String, String>) -> Option<String> {
    q.get("server")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Сериализует пары `HashMap` обратно в query-строку для проксирования.
/// `server` исключается. Минимальный url-encoding. Возвращает строку БЕЗ ведущего `?`.
fn rebuild_query_without_server(q: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(q.len());
    for (k, v) in q.iter() {
        if k == "server" {
            continue;
        }
        let kv = format!("{}={}", urlencode_minimal(k), urlencode_minimal(v));
        parts.push(kv);
    }
    parts.join("&")
}

fn urlencode_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (b'a'..=b'z').contains(&b)
            || (b'A'..=b'Z').contains(&b)
            || (b'0'..=b'9').contains(&b)
            || matches!(b, b'-' | b'_' | b'.' | b'~')
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

async fn close_with_policy_violation(mut socket: WebSocket, reason: &str) {
    let cf = CloseFrame {
        code: 1008,
        reason: std::borrow::Cow::Owned(reason.to_string()),
    };
    let _ = socket.send(Message::Close(Some(cf))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_serialization() {
        let todo = Todo {
            id: "t1".into(),
            root_path: "forge".into(),
            title: "x".into(),
            description: None,
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            plan_mode: false,
            created_at: "2026-05-10T00:00:00.000Z".into(),
            updated_at: "2026-05-10T00:00:00.000Z".into(),
            origin: crate::todos::default_origin_local(),
        };
        let ev = TodoEvent::Upsert { todo };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"upsert\""));
        assert!(s.contains("\"todo\""));
        // Phase 1 — поле переименовано в root_path; project_id больше не пишется.
        assert!(s.contains("\"root_path\":\"forge\""));
        // Phase 3 — origin сериализуется ВСЕГДА, фронт получает унифицированный формат.
        assert!(s.contains("\"origin\":\"local\""));
    }

    #[test]
    fn removed_serialization() {
        let ev = TodoEvent::Removed {
            root_path: "/abs/forge".into(),
            id: "t1".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"removed\""));
        assert!(s.contains("\"root_path\":\"/abs/forge\""));
        assert!(s.contains("\"id\":\"t1\""));
    }

    #[test]
    fn reload_serialization() {
        let ev = TodoEvent::Reload {
            root_path: "/abs/forge".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"reload\""));
        assert!(s.contains("\"root_path\":\"/abs/forge\""));
    }

    #[test]
    fn root_path_extraction() {
        let todo = Todo {
            id: "t1".into(),
            root_path: "/abs/p1".into(),
            title: "x".into(),
            description: None,
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            plan_mode: false,
            created_at: "2026-05-10T00:00:00.000Z".into(),
            updated_at: "2026-05-10T00:00:00.000Z".into(),
            origin: crate::todos::default_origin_local(),
        };
        assert_eq!(TodoEvent::Upsert { todo }.root_path(), "/abs/p1");
        assert_eq!(
            TodoEvent::Removed {
                root_path: "/abs/p2".into(),
                id: "x".into(),
            }
            .root_path(),
            "/abs/p2"
        );
        assert_eq!(
            TodoEvent::Reload {
                root_path: "/abs/p3".into(),
            }
            .root_path(),
            "/abs/p3"
        );
    }
}
