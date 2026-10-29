//! WebSocket `/ws/tracker` — realtime-поток изменений трекера.
//!
//! Устройство дословно повторяет `/ws/todos` хоста
//! (`tmux-web/src/ws_todos.rs`): подписка на broadcast ДО снятия снимка,
//! первым кадром снимок, дальше события, Ping каждые 30 секунд, отставший
//! клиент получает `reload`, а не разрыв. Совпадение намеренное — фронт
//! трекера пишется по образцу уже работающего клиента TODO, и любое
//! расхождение в протоколе пришлось бы держать в голове при каждой правке.
//!
//! # Wire-протокол (сервер → клиент)
//!
//! | кадр | когда |
//! |---|---|
//! | `{"kind":"snapshot", …[`StateDto`]…}` | при подключении и после `reload` |
//! | `{"kind":"task_upsert","task":{…}}` | задача создана или изменена |
//! | `{"kind":"task_removed","id":"…"}` | задача удалена |
//! | `{"kind":"entry_upsert","entry":{…}}` | отрезок начат, остановлен, заведён или поправлен |
//! | `{"kind":"entry_removed","id":"…"}` | отрезок удалён |
//! | `{"kind":"reload"}` | клиенту следует перечитать `GET /api/tracker/state` |
//!
//! Клиент → сервер не шлёт НИЧЕГО, кроме служебных Pong/Close: все мутации
//! идут через REST. Это осознанный отказ от «команд по сокету» — иначе
//! пришлось бы дублировать в WS всю проверку тел запросов, которая в
//! [`crate::routes`] уже есть.
//!
//! # Почему пяти вариантов событий хватает
//!
//! Точечные кадры есть только у задач и отрезков — у сущностей, которые
//! меняются ЧАСТО (перетаскивание в канбане, тикающие таймеры). Списки, пункты
//! чек-листа и вехи меняются редко и рвано, поэтому их мутации публикуются как
//! `reload`; исключение — пункт чек-листа, который вдобавок шлёт `task_upsert`
//! родительской задачи, потому что от него зависит её прогресс `n/m`.
//! Расширять enum ради них означало бы усложнить фронт ради экономии одного
//! запроса в час.
//!
//! # Отставший клиент
//!
//! `broadcast::Receiver::recv` при переполнении буфера (64, см.
//! [`crate::state::EVENT_BUFFER`]) отдаёт `Lagged(n)`. Молча продолжать нельзя:
//! пропущенные события означают, что состояние клиента разошлось с сервером
//! НЕЗАМЕТНО для него. Поэтому в ответ на `Lagged` уходит новый снимок — то
//! есть восстановление, а не просьба «сходи сам».

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;

use crate::dto::StateDto;
use crate::models::{TimeEntry, TrackerTask};
use crate::state::TrackerState;

/// Период heartbeat-пинга. 30 секунд — как у `/ws/todos`.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Событие трекера для broadcast-канала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackerEvent {
    /// Задача создана или изменена.
    TaskUpsert {
        /// Актуальное состояние задачи целиком.
        task: Box<TrackerTask>,
    },
    /// Задача удалена.
    TaskRemoved {
        /// Идентификатор удалённой задачи.
        id: String,
    },
    /// Отрезок времени начат, остановлен, заведён вручную или поправлен.
    EntryUpsert {
        /// Актуальное состояние отрезка.
        entry: Box<TimeEntry>,
    },
    /// Отрезок времени удалён.
    EntryRemoved {
        /// Идентификатор удалённого отрезка.
        id: String,
    },
    /// Изменилось что-то, для чего точечного кадра нет (список, пункт
    /// чек-листа, веха, массовая перестановка): клиенту следует перечитать
    /// состояние целиком.
    Reload,
}

impl TrackerEvent {
    /// Кадр «задача изменилась».
    pub fn task_upsert(task: TrackerTask) -> Self {
        Self::TaskUpsert {
            task: Box::new(task),
        }
    }

    /// Кадр «задача удалена».
    pub fn task_removed(id: impl Into<String>) -> Self {
        Self::TaskRemoved { id: id.into() }
    }

    /// Кадр «отрезок изменился».
    pub fn entry_upsert(entry: TimeEntry) -> Self {
        Self::EntryUpsert {
            entry: Box::new(entry),
        }
    }

    /// Кадр «отрезок удалён».
    pub fn entry_removed(id: impl Into<String>) -> Self {
        Self::EntryRemoved { id: id.into() }
    }
}

/// `GET /ws/tracker` — upgrade в WebSocket.
///
/// Query-параметров нет и не будет: задачи трекера ГЛОБАЛЬНЫЕ, фильтровать
/// поток по корню проекта (как это делает `/ws/todos`) здесь нечем и незачем.
pub async fn tracker_ws(ws: WebSocketUpgrade, State(state): State<Arc<TrackerState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Обработчик одного соединения.
async fn handle_socket(socket: WebSocket, state: Arc<TrackerState>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Подписка ИДЁТ ПЕРВОЙ, до снимка. Обратный порядок терял бы мутации,
    // случившиеся в окне между снимком и подпиской: в снимок они не попали, а
    // подписки ещё не было. При этом порядке худшее, что бывает, — дубль
    // события, которое клиент идемпотентно применит по id.
    let mut rx = state.events.subscribe();

    if !send_snapshot(&ws_tx, &state).await {
        return;
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    // Первый tick срабатывает мгновенно — съедаем его, чтобы не пинговать
    // клиента сразу вслед за снимком.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            biased;

            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_tracker", "ws/tracker: shutdown signalled");
                break;
            }

            ev = rx.recv() => {
                match ev {
                    Ok(event) => {
                        let json = match serde_json::to_string(&event) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!(target: "forge_tracker", error = %e, "ws/tracker: event serialize failed");
                                continue;
                            }
                        };
                        if send_text(&ws_tx, json).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(target: "forge_tracker", lag = n, "ws/tracker: subscriber lagged, resending snapshot");
                        if !send_snapshot(&ws_tx, &state).await {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => {
                        tracing::debug!(target: "forge_tracker", "ws/tracker: broadcast closed");
                        break;
                    }
                }
            }

            _ = heartbeat.tick() => {
                let mut guard = ws_tx.lock().await;
                if guard.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }

            inbound = ws_rx.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(target: "forge_tracker", error = %e, "ws/tracker: recv error");
                        break;
                    }
                }
            }
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
}

/// Кадр снимка: та же форма, что у `GET /api/tracker/state`, плюс `kind`.
///
/// Публичная, потому что на ней держится тест протокола: он проверяет форму
/// кадра, не поднимая настоящее сетевое соединение.
pub fn snapshot_frame(state: &TrackerState) -> serde_json::Value {
    let snapshot = StateDto::from_store(&state.store);
    let mut value = serde_json::to_value(snapshot).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("kind".to_string(), serde_json::json!("snapshot"));
    }
    value
}

/// Отправляет снимок; `false` — соединение можно закрывать.
async fn send_snapshot(
    ws_tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &TrackerState,
) -> bool {
    let frame = snapshot_frame(state).to_string();
    send_text(ws_tx, frame).await.is_ok()
}

/// Отправляет текстовый кадр через общий sink.
async fn send_text(
    ws_tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    text: String,
) -> std::result::Result<(), axum::Error> {
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{TaskStatus, TimeEntry, TrackerTask};

    fn task() -> TrackerTask {
        TrackerTask {
            id: "t1".into(),
            list_id: None,
            title: "написать письмо".into(),
            notes: None,
            status: TaskStatus::Inbox,
            priority: 2,
            tags: vec![],
            estimate_min: None,
            start: None,
            due: None,
            order: 0,
            created_at: "2026-09-08T10:00:00Z".into(),
            updated_at: "2026-09-08T10:00:00Z".into(),
            done_at: None,
        }
    }

    fn entry() -> TimeEntry {
        TimeEntry {
            id: "e1".into(),
            task_id: "t1".into(),
            item_id: None,
            started_at: "2026-09-08T10:00:00Z".into(),
            stopped_at: None,
            note: None,
            source: "timer".into(),
        }
    }

    #[test]
    fn event_kinds_are_snake_case() {
        let cases = [
            (TrackerEvent::task_upsert(task()), "task_upsert"),
            (TrackerEvent::task_removed("t1"), "task_removed"),
            (TrackerEvent::entry_upsert(entry()), "entry_upsert"),
            (TrackerEvent::entry_removed("e1"), "entry_removed"),
            (TrackerEvent::Reload, "reload"),
        ];
        for (event, kind) in cases {
            let raw = serde_json::to_string(&event).expect("event serializes");
            assert!(
                raw.contains(&format!("\"kind\":\"{kind}\"")),
                "{kind} not in {raw}"
            );
        }
    }

    #[test]
    fn task_upsert_carries_the_whole_task() {
        let raw = serde_json::to_string(&TrackerEvent::task_upsert(task())).unwrap();
        assert!(raw.contains("\"title\":\"написать письмо\""));
        assert!(raw.contains("\"status\":\"inbox\""));
    }

    #[test]
    fn events_round_trip() {
        for event in [
            TrackerEvent::task_upsert(task()),
            TrackerEvent::task_removed("t1"),
            TrackerEvent::entry_upsert(entry()),
            TrackerEvent::entry_removed("e1"),
            TrackerEvent::Reload,
        ] {
            let raw = serde_json::to_string(&event).unwrap();
            let back: TrackerEvent = serde_json::from_str(&raw).unwrap();
            assert_eq!(back, event);
        }
    }
}
