//! Phase 3 — WebSocket-handler `/ws/todos` для realtime TODO-стрима.
//!
//! Клиент подключается → сервер отправляет полный `{kind:"snapshot",todos:[...]}`
//! (то же, что вернул бы `GET /api/todos?project_id=...`) → дальше шлёт
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
//! ### Фильтрация по project_id
//!
//! WS-handler принимает query-параметр `project_id`. Если он указан —
//! сервер фильтрует входящие [`TodoEvent`] и форвардит клиенту только те,
//! что относятся к этому проекту. Если `project_id` пуст — берём активный
//! проект из `state.projects` на момент connect.
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

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;

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
/// `{"kind":"upsert","todo":{...}}`, `{"kind":"removed","id":"..."}`,
/// `{"kind":"reload"}`.
///
/// `Snapshot` — отдельный «синтетический» вариант, **не** идущий в broadcast:
/// формируется на стороне handler'а при connect и шлётся напрямую клиенту,
/// чтобы не плодить snapshot-флуд по всем подписчикам.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoEvent {
    /// TODO создана/обновлена. Несёт `project_id` (для фильтрации) и `todo`.
    Upsert { todo: Todo },
    /// TODO удалена. Несёт `project_id` (для фильтрации) и `id`.
    Removed { project_id: String, id: String },
    /// Сигнал клиентам: ресинхронизироваться через `fetchTodos()`.
    Reload { project_id: String },
}

impl TodoEvent {
    /// Возвращает `project_id`, к которому относится событие. Используется
    /// сервером для фильтрации broadcast-стрима по `project_id` подписчика.
    pub fn project_id(&self) -> &str {
        match self {
            TodoEvent::Upsert { todo } => &todo.project_id,
            TodoEvent::Removed { project_id, .. } => project_id,
            TodoEvent::Reload { project_id } => project_id,
        }
    }
}

/// Query-параметры WS-handler'а. `project_id` опционален — если пуст,
/// берём активный проект из `state.projects` на момент connect.
#[derive(Debug, Deserialize)]
pub struct TodoWsQuery {
    #[serde(default)]
    pub project_id: Option<String>,
}

/// `GET /ws/todos?project_id=...` — upgrade в WebSocket, далее [`handle_socket`].
pub async fn todos_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<TodoWsQuery>,
) -> Response {
    let project_id = match q.project_id.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => state.projects.read().await.active().id.clone(),
    };
    tracing::info!(%project_id, "ws/todos upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state, project_id))
}

/// Основной обработчик одного WS-соединения.
async fn handle_socket(socket: WebSocket, state: AppState, project_id: String) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // 1) Snapshot текущего состояния TODO для project_id.
    let todos = state.todos.list(&project_id);
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

            // Broadcast: TodoEvent → JSON Text (с фильтром по project_id).
            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        if event.project_id() != project_id {
                            continue;
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
pub fn removed(project_id: impl Into<String>, id: impl Into<String>) -> TodoEvent {
    TodoEvent::Removed {
        project_id: project_id.into(),
        id: id.into(),
    }
}

/// Helper: сформировать `Reload` событие.
#[allow(dead_code)]
pub fn reload(project_id: impl Into<String>) -> TodoEvent {
    TodoEvent::Reload {
        project_id: project_id.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_serialization() {
        let todo = Todo {
            id: "t1".into(),
            project_id: "forge".into(),
            title: "x".into(),
            description: None,
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            plan_mode: false,
            created_at: "2026-05-10T00:00:00.000Z".into(),
            updated_at: "2026-05-10T00:00:00.000Z".into(),
        };
        let ev = TodoEvent::Upsert { todo };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"upsert\""));
        assert!(s.contains("\"todo\""));
        assert!(s.contains("\"project_id\":\"forge\""));
    }

    #[test]
    fn removed_serialization() {
        let ev = TodoEvent::Removed {
            project_id: "forge".into(),
            id: "t1".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"removed\""));
        assert!(s.contains("\"project_id\":\"forge\""));
        assert!(s.contains("\"id\":\"t1\""));
    }

    #[test]
    fn reload_serialization() {
        let ev = TodoEvent::Reload {
            project_id: "forge".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"reload\""));
        assert!(s.contains("\"project_id\":\"forge\""));
    }

    #[test]
    fn project_id_extraction() {
        let todo = Todo {
            id: "t1".into(),
            project_id: "p1".into(),
            title: "x".into(),
            description: None,
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            plan_mode: false,
            created_at: "2026-05-10T00:00:00.000Z".into(),
            updated_at: "2026-05-10T00:00:00.000Z".into(),
        };
        assert_eq!(TodoEvent::Upsert { todo }.project_id(), "p1");
        assert_eq!(
            TodoEvent::Removed {
                project_id: "p2".into(),
                id: "x".into(),
            }
            .project_id(),
            "p2"
        );
        assert_eq!(
            TodoEvent::Reload {
                project_id: "p3".into(),
            }
            .project_id(),
            "p3"
        );
    }
}
