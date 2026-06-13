//! WebSocket-handler `/ws/tasks?path=...` для realtime
//! task-стрима с привязкой к cwd.
//!
//! Клиент подключается → сервер берёт `?path=` (или fallback на
//! `state.active_path_tx`) → отправляет полный `{kind:"snapshot",data:...}`
//! (тот же JSON, что вернул бы `GET /api/tasks?path=...`) → дальше шлёт
//! `{kind:"upsert",issue:...}` / `{kind:"removed",id:"..."}` по мере того,
//! как per-connection notify watcher детектит изменения
//! `<path>/.beads/issues.jsonl`.
//!
//! После Phase 4 (`remove-projects-concept`) понятие «проект» удалено
//! целиком — query-параметр сменился c `project_id` на `path`.
//!
//! ### Почему per-connection watcher (а не shared broadcast)
//!
//! Multi-tab сценарий: вкладка A на cwd X и вкладка B на cwd Y. Глобальный
//! broadcast пушит события только одного «активного» пути. Per-conn watcher
//! решает проблему: каждое соединение отслеживает ровно тот `.beads/`,
//! который соответствует его `?path=`.
//!
//! Глобальный `tasks_tx` остаётся для `notifier.rs` (он привязан к
//! initial active path и отслеживает только его).
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
//! ### Резолв path
//!
//! Если `path` в query пуст — берём `state.active_path_tx.borrow()`
//! (последний установленный активный путь, по умолчанию — cwd процесса).
//! Иначе используем переданный путь как есть (`PathBuf::from(&path)`).
//!
//! ### Lifecycle
//!
//! Один WS = один notify watcher + один heartbeat-таймер. Disconnect (ws_rx
//! EOF / send error) → дропаем watcher и выходим. Если в проекте нет
//! `.beads/` — снапшот пустой, watcher не запускается, остаётся только
//! heartbeat (чтобы клиент не реконнектился впустую).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep_until, Instant};

use crate::remote_proxy;
use crate::tasks::{self, diff_issues, snapshot};
use crate::tasks_watcher::{find_beads_dir, relevant_event, DEBOUNCE_MS};
use crate::AppState;

/// Heartbeat-период. 30s — стандартный компромисс: достаточно часто, чтобы
/// детектить полу-открытые соединения (NAT timeout, proxy idle), но не
/// настолько часто, чтобы нагружать CPU/сеть.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Query-параметры WS-handler'а. `path` опционален — если пуст,
/// берём текущее значение `state.active_path_tx` на момент connect.
///
/// Handler перешёл на `Query<HashMap<String,String>>` для поддержки
/// `?server=<id>`-прокси; struct сохранён как документация контракта query.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct TasksWsQuery {
    #[serde(default)]
    pub path: Option<String>,
}

/// `GET /ws/tasks?path=...` — upgrade в WebSocket, далее [`handle_socket`].
///
/// ### Поддержка `?server=<id>` (remote proxy)
///
/// Если в query присутствует `server=<id>`:
/// - `state.remote_mode == false` → upgrade + Close{1008, 'remote mode disabled'}.
/// - `state.remote_mode == true`  → upgrade + делегирование в
///   [`remote_proxy::proxy_websocket`] на upstream `/ws/tasks` (с query без `server`).
pub async fn tasks_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response {
    // Phase 4 — remote-proxy ветка.
    if let Some(server_id) = extract_server_id(&raw) {
        if !state.remote_mode {
            tracing::warn!(server_id, "ws/tasks: ?server requested in non-remote mode");
            return ws.on_upgrade(move |socket| close_with_policy_violation(socket, "remote mode disabled"));
        }
        let upstream_query = rebuild_query_without_server(&raw);
        return ws.on_upgrade(move |socket| async move {
            let server = {
                let store = state.remotes.read().await;
                remote_proxy::resolve_server(&store, &server_id)
            };
            let server = match server {
                Ok(s) => s,
                Err(e) => {
                    tracing::trace!(error = %e, server_id, "ws/tasks: unknown remote server");
                    return;
                }
            };
            if let Err(e) = remote_proxy::proxy_websocket(
                &server,
                "/ws/tasks",
                &upstream_query,
                socket,
            )
            .await
            {
                tracing::trace!(error = %e, server_id, "ws/tasks proxy_websocket finished with error");
            }
        });
    }

    // Локальный путь — берём из ?path= или fallback на active_path_tx.
    let path = resolve_active_path(&state, raw.get("path").map(String::as_str));
    tracing::info!(path = %path.display(), "ws/tasks upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, path))
}

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

/// Резолвит `?path=` query → PathBuf.
///
/// - `None` или пусто → последнее значение `state.active_path_tx` (по
///   умолчанию — cwd процесса).
/// - `Some(path)` → как есть.
fn resolve_active_path(state: &AppState, path: Option<&str>) -> PathBuf {
    match path.map(str::trim) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => state.active_path_tx.borrow().clone(),
    }
}

/// Основной обработчик одного WS-соединения.
async fn handle_socket(socket: WebSocket, active_path: PathBuf) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // 1) Поднимаем per-conn notify watcher ДО снятия snapshot. Иначе файловые
    //    мутации `.beads/issues.jsonl`, случившиеся в окне между snapshot и
    //    стартом watcher'а, теряются: snapshot их не видит, а watcher ещё не
    //    подписан на inotify. Watcher сначала → snapshot потом гарантирует, что
    //    любое изменение после baseline будет поймано debounce-diff'ом. Если
    //    нет `.beads/` — остаёмся в heartbeat-only режиме.
    let beads_dir = find_beads_dir(&active_path);
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
            tracing::debug!(path = %active_path.display(), "ws/tasks: no .beads/ found — heartbeat only");
            None
        }
    };

    // 2) Шлём snapshot. Если br не отвечает — отдаём пустой envelope. Snapshot
    //    снимается ПОСЛЕ старта watcher'а: любая мутация после этого момента
    //    придёт через debounce-diff (в худшем случае с дублирующимся upsert,
    //    который клиент идемпотентно применяет по id).
    let snapshot_data = match tasks::list_tasks(&active_path).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, path = %active_path.display(), "ws/tasks: initial list_tasks failed");
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

    // 3) Берём baseline snapshot для последующих diff'ов.
    let mut prev = match snapshot(&active_path).await {
        Ok(s) => s,
        Err(_) => std::collections::HashMap::new(),
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
                match snapshot(&active_path).await {
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
                        // Канал закрыт (watcher дропнут). recv() закрытого
                        // unbounded-канала возвращает None мгновенно на каждой
                        // итерации select → busy-loop 100% CPU. Заменяем rx на
                        // вечно-pending канал (sender забыт), чтобы эта ветка
                        // больше не срабатывала. Heartbeat и inbound остаются;
                        // realtime-обновлений не будет до reconnect клиента.
                        tracing::debug!("ws/tasks: notify channel closed — switching to pending receiver");
                        let (tx, rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
                        std::mem::forget(tx);
                        notify_rx = rx;
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
