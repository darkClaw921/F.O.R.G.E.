//! Phase 6.D — WebSocket-handler `/ws/tasks?project_id=...` для realtime
//! task-стрима с привязкой к конкретному проекту.
//!
//! Клиент подключается → сервер резолвит `project_id` в путь → отправляет
//! полный `{kind:"snapshot",data:...}` (тот же JSON, что вернул бы
//! `GET /api/tasks?project_id=...`) → дальше шлёт `{kind:"upsert",issue:...}`
//! / `{kind:"removed",id:"..."}` по мере того, как per-connection notify
//! watcher детектит изменения `<path>/.beads/issues.jsonl`.
//!
//! ### Почему per-connection watcher (а не shared broadcast)
//!
//! Раньше WS подписывался на глобальный `state.tasks_tx` — тот пушил события
//! только активного проекта. Это ломалось в multi-tab сценарии: вкладка A
//! на проекте X и вкладка B на проекте Y — кто-то получал чужие/пустые
//! события. Per-conn watcher решает проблему: каждое соединение отслеживает
//! ровно тот `.beads/`, который соответствует его `project_id`.
//!
//! Глобальный `tasks_tx` остаётся для `notifier.rs` (он привязан к active
//! project и отслеживает только его).
//!
//! ### Wire-протокол
//!
//! Все сообщения сервер→клиент — `Message::Text` с JSON-тэгом `kind`:
//!
//! - `{kind:"snapshot", data: <br list --json --all --limit 0 result>}` —
//!   при connect.
//! - `{kind:"upsert", issue: {...}}` — задача создана / изменена / закрыта.
//! - `{kind:"removed", id:"..."}` — задача физически удалена из beads БД.
//! - `{kind:"reload"}` — клиенту следует сделать `fetchTasks()` (не
//!   используется в per-conn режиме, оставлено для совместимости).
//!
//! Клиент → сервер: только Pong (axum шлёт автоматически на Ping) и Close.
//! Любые другие frames — игнорируются (warn-log).
//!
//! ### Резолв project_id → path
//!
//! Если `project_id` в query пуст — берём активный проект.
//! Если `project_id` начинается с `__path__:` — extract абсолютный путь.
//! Иначе ищем в `ProjectStore::find_any` (registered + transient).
//!
//! ### Lifecycle
//!
//! Один WS = один notify watcher + один heartbeat-таймер. Disconnect (ws_rx
//! EOF / send error) → дропаем watcher и выходим. Если в проекте нет
//! `.beads/` — снапшот пустой, watcher не запускается, остаётся только
//! heartbeat (чтобы клиент не реконнектился впустую).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep_until, Instant};

use crate::tasks::{self, diff_issues, snapshot};
use crate::tasks_watcher::{find_beads_dir, relevant_event, DEBOUNCE_MS};
use crate::AppState;

/// Heartbeat-период. 30s — стандартный компромисс: достаточно часто, чтобы
/// детектить полу-открытые соединения (NAT timeout, proxy idle), но не
/// настолько часто, чтобы нагружать CPU/сеть.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Query-параметры WS-handler'а. `project_id` опционален — если пуст,
/// берём активный проект из `state.projects` на момент connect.
#[derive(Debug, Deserialize)]
pub struct TasksWsQuery {
    #[serde(default)]
    pub project_id: Option<String>,
}

/// `GET /ws/tasks?project_id=...` — upgrade в WebSocket, далее [`handle_socket`].
pub async fn tasks_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<TasksWsQuery>,
) -> Response {
    let path = resolve_project_path(&state, q.project_id.as_deref()).await;
    tracing::info!(path = %path.display(), "ws/tasks upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, path))
}

/// Резолвит `project_id` query → path.
///
/// - `None` или пусто → active project.
/// - `__path__:<abs>` → `<abs>` (transient project).
/// - Иначе ищем в `ProjectStore::find_any`. Если не нашли — fallback на active.
async fn resolve_project_path(state: &AppState, project_id: Option<&str>) -> PathBuf {
    let store = state.projects.read().await;
    match project_id {
        Some(id) if !id.is_empty() => {
            if let Some(rest) = id.strip_prefix("__path__:") {
                return PathBuf::from(rest);
            }
            if let Some(p) = store.find_any(id) {
                return p.path.clone();
            }
            tracing::warn!(
                %id,
                "ws/tasks: project_id not found in store, falling back to active"
            );
            store.active().path.clone()
        }
        _ => store.active().path.clone(),
    }
}

/// Основной обработчик одного WS-соединения.
async fn handle_socket(socket: WebSocket, project_path: PathBuf) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // 1) Шлём snapshot. Если br не отвечает — отдаём пустой envelope.
    let snapshot_data = match tasks::list_tasks(&project_path).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, path = %project_path.display(), "ws/tasks: initial list_tasks failed");
            serde_json::json!({"issues": [], "total": 0})
        }
    };
    let snapshot_msg = serde_json::json!({
        "kind": "snapshot",
        "data": snapshot_data,
    });
    if let Err(e) = send_text(&ws_tx, snapshot_msg.to_string()).await {
        tracing::debug!(error = ?e, "ws/tasks: snapshot send failed; closing");
        return;
    }

    // 2) Берём baseline snapshot для последующих diff'ов.
    let mut prev = match snapshot(&project_path).await {
        Ok(s) => s,
        Err(_) => std::collections::HashMap::new(),
    };

    // 3) Поднимаем per-conn notify watcher если есть `.beads/`. Если нет —
    //    остаёмся в heartbeat-only режиме (клиент получит пустой snapshot
    //    и не будет реконнектиться впустую).
    let beads_dir = find_beads_dir(&project_path);
    let (notify_tx, mut notify_rx) =
        mpsc::unbounded_channel::<notify::Result<notify::Event>>();
    let _watcher: Option<RecommendedWatcher> = match beads_dir.as_ref() {
        Some(dir) => {
            match notify::recommended_watcher(
                move |res: notify::Result<notify::Event>| {
                    let _ = notify_tx.send(res);
                },
            ) {
                Ok(mut w) => match w.watch(dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!(path = %dir.display(), "ws/tasks: watcher started");
                        Some(w)
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, path = %dir.display(), "ws/tasks: watch failed");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(error = ?e, "ws/tasks: failed to create notify watcher");
                    None
                }
            }
        }
        None => {
            tracing::debug!(path = %project_path.display(), "ws/tasks: no .beads/ found — heartbeat only");
            None
        }
    };

    // 4) Heartbeat таймер.
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    // 5) Debounce-deadline для группировки burst'ов notify-событий.
    let mut debounce_deadline: Option<Instant> = None;

    // 6) Главный select-loop.
    loop {
        let timer = debounce_deadline.map(sleep_until);

        tokio::select! {
            biased;

            // Истечение debounce → snapshot + diff + send.
            _ = async {
                if let Some(t) = timer { t.await }
                else { std::future::pending::<()>().await }
            } => {
                debounce_deadline = None;
                match snapshot(&project_path).await {
                    Ok(new_snap) => {
                        let events = diff_issues(&prev, &new_snap);
                        for ev in events {
                            let json = match serde_json::to_string(&ev) {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::warn!(error = ?e, "TaskEvent serialize failed");
                                    continue;
                                }
                            };
                            if let Err(e) = send_text(&ws_tx, json).await {
                                tracing::debug!(error = ?e, "ws/tasks: event send failed; closing");
                                break;
                            }
                        }
                        prev = new_snap;
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, "ws/tasks: snapshot failed during debounce");
                    }
                }
            }

            // Notify-событие → стартуем/продлеваем debounce.
            event = notify_rx.recv() => {
                match event {
                    None => {
                        // Канал закрыт (watcher дропнут). Продолжаем — heartbeat
                        // и inbound остаются. Без watcher'а просто не будет
                        // realtime-обновлений до reconnect клиента.
                        tracing::debug!("ws/tasks: notify channel closed");
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = ?e, "ws/tasks: notify error event");
                    }
                    Some(Ok(ev)) => {
                        if relevant_event(&ev) {
                            debounce_deadline =
                                Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                        }
                    }
                }
            }

            // Heartbeat: Ping каждые 30s.
            _ = heartbeat.tick() => {
                let mut guard = ws_tx.lock().await;
                if let Err(e) = guard.send(Message::Ping(Vec::new())).await {
                    tracing::debug!(error = ?e, "ws/tasks: ping failed; closing");
                    break;
                }
            }

            // Inbound: ожидаем Pong / Close, остальное — игнор.
            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!("ws/tasks: client closed");
                        break;
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {}
                    Some(Ok(other)) => {
                        tracing::debug!(?other, "ws/tasks: unexpected inbound message; ignored");
                    }
                    Some(Err(e)) => {
                        tracing::debug!(error = ?e, "ws/tasks: ws recv error");
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
    tracing::info!("ws/tasks session terminated");
}

/// Отправить Text-фрейм через shared sink. Возвращает Err при сетевом фейле.
async fn send_text(
    ws_tx: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    text: String,
) -> Result<(), axum::Error> {
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}
