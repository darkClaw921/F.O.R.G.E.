//! WebSocket-handler `/ws/echo?conversation_id=&token=`.
//!
//! ## Жизненный цикл одного соединения
//!
//! 1. `echo_ws` upgrade'ит HTTP в WebSocket. Bearer-auth (если включён) уже
//!    выполнен middleware'ом — query-параметр `token` присутствует **только
//!    для удобства клиентского ws-кода** (header Authorization трудно
//!    прокинуть в браузерный WebSocket); сам auth-check делает host-уровень.
//! 2. `handle_socket` запускает 3 logical task'и в одном `tokio::select!`:
//!    - reader: парсит входящие `ClientMsg`, реагирует на `UserMessage` /
//!      `Cancel` / `ActionInvoke` / `Pong`.
//!    - broadcast subscriber: получает `ServerEvent` из общего канала и
//!      форвардит в свой socket если `conversation_id` совпадает (или
//!      событие broadcast'ное).
//!    - heartbeat: каждые 15с шлёт `ServerMsg::Ping`. Клиент отвечает
//!      `Pong`; если в течение `IDLE_TIMEOUT` (60с) не было входящих
//!      сообщений — закрываем.
//! 3. `UserMessage` → инсёрт user-message в БД → `prompt_builder::build` →
//!    `ClaudeRunner::stream`. Поток событий конвертируется в
//!    `ServerMsg::AssistantChunk` и шлётся через broadcast. По завершении —
//!    инсёрт assistant-сообщения с usage в БД + `stats::add_tokens` для
//!    минутного bucket'а + `ServerMsg::AssistantDone` + `StatsUpdate`.

pub mod protocol;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::actions::{self, executor::InvokeResult};
use crate::claude::events::ClaudeEvent;
use crate::claude::prompt_builder::{self, CtxOpts};
use crate::claude::RunRequest;
use crate::db::repo::{chats, messages, stats};
use crate::state::{EchoState, ServerEvent};
use crate::ws::protocol::{ChunkKind, ClientMsg, CtxOptsWire, ServerMsg};

/// Heartbeat-пинг: каждые 15с. Короче, чем у `ws_tasks` (30с) — Echo
/// streaming-чувствителен, реконнект должен быть быстрее.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// Если 60с нет ни одного входящего фрейма (включая pong) — закрываем.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Длина sliding-window'а для rate-limit'а user_message.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// Sliding-window rate-limiter per-WS для `user_message`.
///
/// Хранит timestamps последних N user_message-событий и при попытке
/// «потратить» новый слот сначала эвиктит протухшие (>60s старше now).
/// Если оставшихся ≥ `limit` — отказ. `limit == 0` означает «лимит
/// отключён» (используется в integration-тестах, где сценарий специально
/// шлёт >30 сообщений за секунду).
#[derive(Debug)]
struct RateLimiter {
    window: VecDeque<Instant>,
    limit: u32,
}

impl RateLimiter {
    fn new(limit: u32) -> Self {
        Self {
            window: VecDeque::new(),
            limit,
        }
    }

    /// Пытается зарегистрировать новый user_message. Возвращает `true` если
    /// разрешено (event записан в окно); `false` если лимит превышен.
    fn try_acquire(&mut self) -> bool {
        if self.limit == 0 {
            return true;
        }
        let now = Instant::now();
        // Эвикция: всё старше окна — выкидываем.
        while let Some(front) = self.window.front() {
            if now.duration_since(*front) >= RATE_LIMIT_WINDOW {
                self.window.pop_front();
            } else {
                break;
            }
        }
        if (self.window.len() as u32) >= self.limit {
            return false;
        }
        self.window.push_back(now);
        true
    }
}

/// Query `/ws/echo?conversation_id=<uuid>&token=<bearer>`.
#[derive(Debug, Deserialize)]
pub struct EchoWsQuery {
    pub conversation_id: String,
    /// Опциональный bearer — используется фронтендом который не может
    /// положить Authorization-header в browser-WebSocket. Реальный auth
    /// делает middleware на уровне хоста (см. tmux-web/src/auth.rs).
    #[serde(default)]
    pub token: Option<String>,
}

/// HTTP-handler upgrade'а в WebSocket. Auth уже выполнен middleware'ом.
pub async fn echo_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<EchoState>>,
    Query(q): Query<EchoWsQuery>,
) -> Response {
    tracing::info!(conversation_id = %q.conversation_id, "echo_ws: upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state, q.conversation_id))
}

/// Конвертирует [`ClaudeEvent`] → [`ChunkKind`] для wire-протокола.
fn event_to_chunk_kind(ev: &ClaudeEvent) -> Option<ChunkKind> {
    match ev {
        ClaudeEvent::TextDelta { .. } => Some(ChunkKind::Text),
        ClaudeEvent::Thinking { .. } => Some(ChunkKind::Thinking),
        ClaudeEvent::ToolUse { .. } => Some(ChunkKind::ToolUse),
        _ => None,
    }
}

/// Извлекает текстовую дельту из любого «чанкового» события.
fn event_to_delta(ev: &ClaudeEvent) -> String {
    match ev {
        ClaudeEvent::TextDelta { text } | ClaudeEvent::Thinking { text } => text.clone(),
        ClaudeEvent::ToolUse { name, input } => {
            // Сериализуем tool_use компактно — UI отрендерит «used <name>(...)».
            format!(
                "{} {}",
                name,
                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
            )
        }
        _ => String::new(),
    }
}

/// Главный обработчик одного WS-соединения.
async fn handle_socket(socket: WebSocket, state: Arc<EchoState>, conversation_id: String) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));
    let mut broadcast_rx = state.broadcast.subscribe();

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await; // первый tick немедленный — пропускаем.
    let mut last_activity = Instant::now();

    // Phase 6 — sliding-window rate-limiter, локальный для этого WS.
    // Лимит берём из config'а; 0 = отключено (для интеграционных тестов).
    let mut rate_limiter = RateLimiter::new(state.config.user_message_rate_limit_per_min);

    loop {
        let idle_deadline = last_activity + IDLE_TIMEOUT;
        tokio::select! {
            biased;

            // Idle timeout — закрываем соединение.
            _ = tokio::time::sleep_until(idle_deadline) => {
                tracing::info!(conversation_id, "echo_ws: idle timeout, closing");
                break;
            }

            // Heartbeat ping.
            _ = heartbeat.tick() => {
                if send_server_msg(&ws_tx, &ServerMsg::Ping).await.is_err() {
                    break;
                }
            }

            // Broadcast → ws (если conversation_id совпадает или broadcast).
            ev = broadcast_rx.recv() => {
                match ev {
                    Ok(ev) => {
                        let matches = match &ev.conversation_id {
                            Some(cid) => cid == &conversation_id,
                            None => true,
                        };
                        if matches && send_server_msg(&ws_tx, &ev.msg).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Этот subscriber отстал и пропустил n событий (медленный
                        // клиент / burst чанков обогнал буфер на 256). Раньше мы
                        // молча роняли события → клиент видел оборванный/неполный
                        // ответ навсегда. Теперь шлём Resync — клиент перечитает
                        // переписку через REST и восстановит целостность.
                        tracing::warn!(skipped = n, conversation_id, "echo_ws: broadcast lagged, sending resync");
                        if send_server_msg(&ws_tx, &ServerMsg::Resync).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(conversation_id, "echo_ws: broadcast closed");
                        break;
                    }
                }
            }

            // Inbound ClientMsg.
            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!(conversation_id, "echo_ws: client closed");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_activity = Instant::now();
                        match serde_json::from_str::<ClientMsg>(&text) {
                            Ok(msg) => {
                                // Phase 6 rate-limit: только UserMessage учитывается.
                                // ActionInvoke/Cancel/Pong идут без лимита (системные).
                                if matches!(msg, ClientMsg::UserMessage { .. })
                                    && !rate_limiter.try_acquire()
                                {
                                    tracing::warn!(
                                        target: "forge_echo",
                                        conversation_id,
                                        limit = state.config.user_message_rate_limit_per_min,
                                        "echo_ws: user_message rate-limited"
                                    );
                                    let err = ServerMsg::Error {
                                        code: "rate_limited".into(),
                                        message: format!(
                                            "{} messages per minute exceeded",
                                            state.config.user_message_rate_limit_per_min
                                        ),
                                    };
                                    let _ = send_server_msg(&ws_tx, &err).await;
                                } else {
                                    handle_client_msg(msg, &state, &conversation_id).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(target: "forge_echo", error = %e, raw = %text, "echo_ws: malformed ClientMsg");
                                let err = ServerMsg::Error {
                                    code: "bad_request".into(),
                                    message: format!("malformed message: {e}"),
                                };
                                let _ = send_server_msg(&ws_tx, &err).await;
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {
                        last_activity = Instant::now();
                    }
                    Some(Ok(_)) => {
                        // Binary etc — игнорируем.
                        last_activity = Instant::now();
                    }
                    Some(Err(e)) => {
                        tracing::debug!(error = ?e, "echo_ws: ws recv error");
                        break;
                    }
                }
            }
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::info!(conversation_id, "echo_ws: terminated");
}

/// Отправить ServerMsg как Text-frame.
async fn send_server_msg(
    ws_tx: &Arc<
        Mutex<
            futures_util::stream::SplitSink<WebSocket, Message>,
        >,
    >,
    msg: &ServerMsg,
) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "echo_ws: ServerMsg serialize failed");
            return Ok(());
        }
    };
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

/// Диспатч одного ClientMsg.
async fn handle_client_msg(msg: ClientMsg, state: &Arc<EchoState>, conversation_id: &str) {
    match msg {
        ClientMsg::Pong => {
            // last_activity уже обновлён в caller'е.
        }
        ClientMsg::Cancel { run_id } => {
            let ok = state.runner.cancel(&run_id).await;
            tracing::info!(run_id, cancelled = ok, "echo_ws: cancel");
        }
        ClientMsg::ActionInvoke { action_id, params } => {
            tracing::info!(action_id, ?params, "echo_ws: action_invoke");
            let conv_id = conversation_id.to_string();
            let state_clone = state.clone();
            tokio::spawn(async move {
                handle_action_invoke(state_clone, conv_id, action_id, params).await;
            });
        }
        ClientMsg::UserMessage {
            text,
            conversation_id: msg_conv_id,
            model,
            ctx_opts,
        } => {
            // conversation_id из ClientMsg может отличаться от query (например,
            // юзер переключил чат). Используем тот, что в сообщении —
            // фронтенд знает, к какому чату принадлежит реплика.
            let effective_conv = if msg_conv_id.is_empty() {
                conversation_id.to_string()
            } else {
                msg_conv_id
            };

            // Не блокируем select-loop — спавним работу.
            let state_clone = state.clone();
            tokio::spawn(async move {
                run_user_message(state_clone, effective_conv, text, model, ctx_opts).await;
            });
        }
    }
}

/// Полный цикл обработки user_message: insert → prompt → stream → record.
async fn run_user_message(
    state: Arc<EchoState>,
    conversation_id: String,
    user_text: String,
    model: Option<String>,
    ctx_opts: Option<CtxOptsWire>,
) {
    // Гарантируем существование chat-сессии (для тестов: фронтенд должен
    // сам создавать чат через POST /api/echo/conversations; если по какой-то
    // причине его нет — НЕ создаём, инсёрт user-message упадёт с FK ошибкой
    // и юзер получит явный error.
    if let Ok(None) = chats::get(&state.db, &conversation_id).await {
        let err = ServerMsg::Error {
            code: "no_conversation".into(),
            message: format!("conversation_id {conversation_id} not found"),
        };
        let _ = state
            .broadcast
            .send(ServerEvent::to_conversation(conversation_id.clone(), err));
        return;
    }

    let run_id = uuid::Uuid::new_v4().to_string();

    // 1) Записать user-message.
    if let Err(e) = messages::insert(
        &state.db,
        &conversation_id,
        "user",
        &user_text,
        None,
        None,
        0,
        0,
        0,
        0,
    )
    .await
    {
        tracing::error!(error = %e, "run_user_message: failed to insert user msg");
        let err = ServerMsg::Error {
            code: "db_error".into(),
            message: format!("failed to save user message: {e}"),
        };
        let _ = state
            .broadcast
            .send(ServerEvent::to_conversation(conversation_id.clone(), err));
        return;
    }
    let _ = chats::touch_updated(&state.db, &conversation_id).await;

    // 2) Построить prompt.
    let host = match state.host.get() {
        Some(h) => h.clone(),
        None => {
            tracing::error!("run_user_message: HostApi not initialized");
            let err = ServerMsg::Error {
                code: "server_error".into(),
                message: "host adapter missing".into(),
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id.clone(), err));
            return;
        }
    };

    let opts = match ctx_opts {
        Some(w) => CtxOpts {
            include_pane_capture: w.include_pane_capture,
            project_id: w.project_id,
            include_memories: w.include_memories,
            capture_lines: w.capture_lines,
            session_filter: w.session_filter,
        },
        None => CtxOpts::default(),
    };

    let prompt = match prompt_builder::build(&user_text, &opts, host.as_ref(), &state.db).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "run_user_message: prompt_builder failed");
            let err = ServerMsg::Error {
                code: "prompt_error".into(),
                message: format!("prompt builder failed: {e}"),
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id.clone(), err));
            return;
        }
    };

    // 3) Стрим.
    let req = RunRequest {
        prompt,
        model,
        system: None,
        run_id: run_id.clone(),
    };
    let mut rx = state.runner.stream(req).await;

    let mut assistant_text = String::new();
    let mut final_usage = crate::claude::events::Usage::default();
    let mut had_result = false;
    let mut had_error = false;

    while let Some(ev) = rx.recv().await {
        match &ev {
            ClaudeEvent::TextDelta { text } => assistant_text.push_str(text),
            ClaudeEvent::Result { usage, is_error, .. } => {
                final_usage = usage.clone();
                had_result = true;
                // Result с is_error — это тоже ошибка для клиента.
                if *is_error {
                    had_error = true;
                    let err = ServerMsg::Error {
                        code: "claude_error".into(),
                        message: "Claude finished with an error result".into(),
                    };
                    let _ = state.broadcast.send(ServerEvent::to_conversation(
                        conversation_id.clone(),
                        err,
                    ));
                }
            }
            ClaudeEvent::Error { message } => {
                tracing::warn!(run_id, %message, "stream error event");
                had_error = true;
                // Раньше ошибка только логировалась → клиент молча видел
                // пустой ответ. Теперь явно шлём ServerMsg::Error.
                let err = ServerMsg::Error {
                    code: "claude_error".into(),
                    message: message.clone(),
                };
                let _ = state.broadcast.send(ServerEvent::to_conversation(
                    conversation_id.clone(),
                    err,
                ));
            }
            _ => {}
        }

        // Чанк → broadcast.
        if let Some(kind) = event_to_chunk_kind(&ev) {
            let delta = event_to_delta(&ev);
            let chunk = ServerMsg::AssistantChunk {
                run_id: run_id.clone(),
                kind,
                delta,
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id.clone(), chunk));
        }
    }

    // 4) Финализация: insert assistant + stats.
    //
    // Не пишем пустой assistant-message, если ничего не пришло (текст пуст И
    // не было финального result-event'а). Это случай оборванного/ошибочного
    // стрима (Error без текста, kill по таймауту): раньше в БД оседала пустая
    // assistant-запись, замусоривая историю чата. Клиент уже получил
    // ServerMsg::Error выше. Если текст есть ИЛИ был result (даже пустой,
    // но валидный финал) — сохраняем как обычно.
    let should_persist = !assistant_text.is_empty() || had_result;
    let inserted_id = if should_persist {
        match messages::insert(
            &state.db,
            &conversation_id,
            "assistant",
            &assistant_text,
            None,
            None,
            final_usage.input_tokens as i64,
            final_usage.output_tokens as i64,
            final_usage.cache_creation_input_tokens as i64,
            final_usage.cache_read_input_tokens as i64,
        )
        .await
        {
            Ok(m) => m.id,
            Err(e) => {
                tracing::error!(error = %e, "run_user_message: insert assistant failed");
                String::new()
            }
        }
    } else {
        tracing::debug!(run_id, had_error, "run_user_message: empty stream, skipping assistant insert");
        String::new()
    };
    let _ = chats::touch_updated(&state.db, &conversation_id).await;

    if had_result {
        let now = chrono::Utc::now().timestamp();
        let _ = stats::add_tokens(
            &state.db,
            now,
            final_usage.input_tokens as i64,
            final_usage.output_tokens as i64,
            final_usage.cache_creation_input_tokens as i64,
            final_usage.cache_read_input_tokens as i64,
        )
        .await;
        let _ = state.broadcast.send(ServerEvent::to_conversation(
            conversation_id.clone(),
            ServerMsg::StatsUpdate {
                tokens_in_per_min: final_usage.input_tokens,
                tokens_out_per_min: final_usage.output_tokens,
            },
        ));
    }

    let done = ServerMsg::AssistantDone {
        run_id: run_id.clone(),
        usage: final_usage,
        message_id: inserted_id.clone(),
    };
    let _ = state
        .broadcast
        .send(ServerEvent::to_conversation(conversation_id.clone(), done));

    // Phase 5b — извлечь forge-actions из финального текста и зарегистрировать.
    if !inserted_id.is_empty() {
        let parsed = actions::parser::extract(&assistant_text);
        if !parsed.is_empty() {
            let descriptors = state.register_actions(&inserted_id, parsed).await;
            let buttons = ServerMsg::ActionButtons {
                message_id: inserted_id,
                actions: descriptors,
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id, buttons));
        }
    }
}

/// Обработать `ClientMsg::ActionInvoke` — найти Action в registry и
/// инвокнуть через [`actions::executor::invoke`]. На результат шлём
/// `ServerMsg::Notification` (Ok/Error/Prompt → новое `user_message`).
async fn handle_action_invoke(
    state: Arc<EchoState>,
    conversation_id: String,
    action_id: String,
    _params: serde_json::Value,
) {
    let action = match state.find_action(&action_id).await {
        Some(a) => a,
        None => {
            let msg = ServerMsg::Notification {
                level: crate::ws::protocol::NotificationLevel::Warn,
                title: "Action expired".into(),
                body: format!("action {action_id} not found or expired"),
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id, msg));
            return;
        }
    };

    let host = match state.host.get() {
        Some(h) => h.clone(),
        None => {
            tracing::error!("handle_action_invoke: HostApi not initialized");
            return;
        }
    };

    // From WS — пользовательский контекст (autonomous=false).
    let res = actions::executor::invoke(&action, host, false).await;
    match res {
        Ok(InvokeResult::Prompt { text }) => {
            // Эмулируем новый user_message — но не блокируем select-loop.
            let state_clone = state.clone();
            let conv = conversation_id.clone();
            tokio::spawn(async move {
                run_user_message(state_clone, conv, text, None, None).await;
            });
        }
        Ok(InvokeResult::Ok) => {
            let n = ServerMsg::Notification {
                level: crate::ws::protocol::NotificationLevel::Info,
                title: "Action done".into(),
                body: format!("{} completed", action.label()),
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id, n));
        }
        Ok(InvokeResult::Error { msg }) => {
            let n = ServerMsg::Notification {
                level: crate::ws::protocol::NotificationLevel::Error,
                title: "Action failed".into(),
                body: msg,
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id, n));
        }
        Err(e) => {
            let n = ServerMsg::Notification {
                level: crate::ws::protocol::NotificationLevel::Error,
                title: "Action rejected".into(),
                body: format!("{e}"),
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conversation_id, n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use std::path::PathBuf;

    async fn make_state() -> Arc<EchoState> {
        // ClaudeRunner на отсутствующем CLI — для unit-теста event-конвертеров
        // и broadcast этого достаточно.
        let runner = Arc::new(ClaudeRunner::new(PathBuf::from("/nope"), 1));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        Arc::new(EchoState::new(Arc::new(db), runner))
    }

    #[test]
    fn rate_limiter_allows_up_to_limit_then_blocks() {
        let mut rl = RateLimiter::new(3);
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        // 4-й — отказ.
        assert!(!rl.try_acquire());
        assert!(!rl.try_acquire());
    }

    #[test]
    fn rate_limiter_zero_means_unlimited() {
        let mut rl = RateLimiter::new(0);
        for _ in 0..1_000 {
            assert!(rl.try_acquire());
        }
    }

    #[test]
    fn rate_limiter_evicts_old_entries_when_window_passed() {
        let mut rl = RateLimiter::new(2);
        // Симулируем, что 2 события уже зашли:
        let in_window = Instant::now() - Duration::from_secs(120); // далеко за окном
        rl.window.push_back(in_window);
        rl.window.push_back(in_window);
        // На try_acquire оба должны эвиктнуться, новый — пройти.
        assert!(rl.try_acquire());
        assert_eq!(rl.window.len(), 1);
    }

    #[test]
    fn rate_limiter_thirty_per_minute_default_scenario() {
        // 30 проходят, 31-й — отказ.
        let mut rl = RateLimiter::new(30);
        for _ in 0..30 {
            assert!(rl.try_acquire());
        }
        assert!(!rl.try_acquire(), "31st must be blocked");
    }

    #[test]
    fn event_to_chunk_kind_maps_correctly() {
        assert_eq!(
            event_to_chunk_kind(&ClaudeEvent::TextDelta { text: "x".into() }),
            Some(ChunkKind::Text)
        );
        assert_eq!(
            event_to_chunk_kind(&ClaudeEvent::Thinking { text: "x".into() }),
            Some(ChunkKind::Thinking)
        );
        assert_eq!(
            event_to_chunk_kind(&ClaudeEvent::ToolUse {
                name: "n".into(),
                input: serde_json::json!({}),
            }),
            Some(ChunkKind::ToolUse)
        );
        assert!(event_to_chunk_kind(&ClaudeEvent::Result {
            usage: Default::default(),
            is_error: false,
            raw_json: serde_json::Value::Null
        })
        .is_none());
        assert!(event_to_chunk_kind(&ClaudeEvent::Error {
            message: "e".into()
        })
        .is_none());
    }

    #[test]
    fn event_to_delta_extracts_text() {
        assert_eq!(
            event_to_delta(&ClaudeEvent::TextDelta { text: "hi".into() }),
            "hi"
        );
        assert_eq!(
            event_to_delta(&ClaudeEvent::Thinking { text: "th".into() }),
            "th"
        );
        let s = event_to_delta(&ClaudeEvent::ToolUse {
            name: "Bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        });
        assert!(s.starts_with("Bash "));
        assert!(s.contains("ls"));
    }

    #[tokio::test]
    async fn make_state_works() {
        let _s = make_state().await;
    }

    #[tokio::test]
    async fn run_user_message_errors_on_missing_conversation() {
        let state = make_state().await;
        let mut rx = state.broadcast.subscribe();
        run_user_message(
            state.clone(),
            "missing-conv-id".into(),
            "hi".into(),
            None,
            None,
        )
        .await;
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev.msg {
            ServerMsg::Error { code, .. } => assert_eq!(code, "no_conversation"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
