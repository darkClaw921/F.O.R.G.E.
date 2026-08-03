//! WebSocket-handler `/ws/vk?token=`.
//!
//! ## Жизненный цикл соединения
//!
//! 1. [`vk_ws`] апгрейдит HTTP в WebSocket. Bearer-auth (в remote-mode) уже
//!    выполнен middleware'ом хоста — query-параметр `token` существует только
//!    потому, что браузерный `WebSocket` не умеет слать заголовок
//!    `Authorization`.
//! 2. [`handle_socket`] сразу подписывается на broadcast и **первым кадром**
//!    отправляет текущий `login_state` — клиенту не нужно ждать события,
//!    чтобы отрисовать состояние логина.
//! 3. Дальше — один `tokio::select!`:
//!    - событие из broadcast → кадр клиенту (БЕЗ фильтрации, см. ниже);
//!    - входящий кадр → парсим [`VkClientMsg`], `Pong` сбрасывает
//!      idle-таймер;
//!    - тик heartbeat'а (15с) → [`VkServerMsg::Ping`];
//!    - тишина [`IDLE_TIMEOUT`] (60с) → закрываем соединение;
//!    - `shutdown`-токен → закрываем соединение.
//! 4. `RecvError::Lagged(n)` НЕ рвёт соединение: клиенту уходит
//!    [`VkServerMsg::Resync`], он перечитывает состояние по REST.
//!
//! ## Три инварианта, которые легко потерять при переписывании
//!
//! - **Подписка ДО снапшота.** Событие, случившееся между снапшотом и
//!   подпиской, иначе потерялось бы навсегда.
//! - **`biased` в `select!`.** Порядок веток — это приоритет: shutdown
//!   первым, чтобы остановка сервера не ждала heartbeat'а.
//! - **Битый входящий кадр не рвёт соединение.** Неизвестный кадр может
//!   прийти от более новой версии фронта (или от старой из кэша PWA).
//!
//! ## Почему нет фильтрации по активному диалогу
//!
//! Echo фильтрует события по `conversation_id`, потому что чужой чат клиенту
//! не нужен. Здесь наоборот: unread-бейдж на кнопке Ⓥ и тосты о новых
//! сообщениях должны работать, когда открыта ЛЮБАЯ вкладка. Поэтому сервер
//! вещает все события, а решает, что показать, фронтенд.

pub mod protocol;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::state::VkState;
use crate::ws::protocol::{VkClientMsg, VkServerMsg};

/// Период heartbeat-пинга. Совпадает с Echo и Telegram (15с): realtime-вкладка
/// должна замечать разрыв быстро, чтобы фронт успел переподключиться.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Если 60с не пришло ни одного кадра (включая `pong`) — соединение считается
/// мёртвым и закрывается. Четыре пропущенных пинга подряд.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Query `/ws/vk?token=<bearer>`.
#[derive(Debug, Default, Deserialize)]
pub struct VkWsQuery {
    /// Опциональный bearer для браузерного WebSocket. Проверку делает
    /// middleware хоста, здесь параметр только принимается и игнорируется.
    #[serde(default)]
    pub token: Option<String>,
}

/// HTTP-handler апгрейда в WebSocket.
pub async fn vk_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<VkState>>,
    Query(_q): Query<VkWsQuery>,
) -> Response {
    tracing::debug!(target: "forge_vk", "vk_ws: upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Тип отправляющей половины сокета под общим Mutex'ом.
type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

/// Главный цикл одного WS-соединения.
async fn handle_socket(socket: WebSocket, state: Arc<VkState>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(Mutex::new(ws_tx));

    // Подписка ДО отправки снапшота: событие, случившееся между снапшотом и
    // подпиской, иначе потерялось бы навсегда.
    let mut broadcast_rx = state.subscribe();

    let snapshot = VkServerMsg::LoginState(state.login_state_dto().await);
    if send_server_msg(&ws_tx, &snapshot).await.is_err() {
        return;
    }

    // Тайминги берутся из состояния, а не из констант: в проде они равны
    // HEARTBEAT_INTERVAL/IDLE_TIMEOUT, а интеграционный тест поднимает
    // состояние с сотнями миллисекунд и проверяет настоящий цикл, не дожидаясь
    // боевой минуты (см. VkState::with_ws_timings).
    let mut heartbeat = tokio::time::interval(state.ws_heartbeat);
    heartbeat.tick().await; // первый тик мгновенный — пропускаем.
    let mut last_activity = Instant::now();

    loop {
        let idle_deadline = last_activity + state.ws_idle_timeout;
        tokio::select! {
            biased;

            // Остановка сервера — закрываемся первыми.
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_vk", "vk_ws: shutdown, closing");
                break;
            }

            // Idle-таймаут.
            _ = tokio::time::sleep_until(idle_deadline) => {
                tracing::info!(target: "forge_vk", "vk_ws: idle timeout, closing");
                break;
            }

            // Heartbeat.
            _ = heartbeat.tick() => {
                if send_server_msg(&ws_tx, &VkServerMsg::Ping).await.is_err() {
                    break;
                }
            }

            // Событие из broadcast — без фильтрации.
            ev = broadcast_rx.recv() => {
                match ev {
                    Ok(msg) => {
                        if send_server_msg(&ws_tx, &msg).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            target: "forge_vk",
                            skipped = n,
                            "vk_ws: broadcast lagged, sending resync"
                        );
                        if send_server_msg(&ws_tx, &VkServerMsg::Resync).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(target: "forge_vk", "vk_ws: broadcast closed");
                        break;
                    }
                }
            }

            // Входящий кадр.
            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!(target: "forge_vk", "vk_ws: client closed");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_activity = Instant::now();
                        match serde_json::from_str::<VkClientMsg>(&text) {
                            Ok(VkClientMsg::Pong) => {}
                            Err(e) => {
                                // Не рвём соединение: неизвестный кадр может
                                // прийти от более новой версии фронта.
                                tracing::warn!(
                                    target: "forge_vk",
                                    error = %e,
                                    raw = %text,
                                    "vk_ws: malformed client frame, ignored"
                                );
                            }
                        }
                    }
                    // Протокольные ping/pong тоже считаем активностью.
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(_)) => {
                        last_activity = Instant::now();
                    }
                    Some(Err(e)) => {
                        tracing::debug!(target: "forge_vk", error = ?e, "vk_ws: recv error");
                        break;
                    }
                }
            }
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::debug!(target: "forge_vk", "vk_ws: terminated");
}

/// Отправляет [`VkServerMsg`] текстовым кадром.
///
/// Ошибка сериализации не должна рвать соединение (это баг сервера, а не
/// клиента), поэтому логируется и трактуется как успех.
async fn send_server_msg(ws_tx: &WsSink, msg: &VkServerMsg) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "forge_vk", error = %e, "vk_ws: serialize failed");
            return Ok(());
        }
    };
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_is_shorter_than_idle_timeout() {
        // Иначе соединение умирало бы раньше, чем клиент успевал ответить.
        assert!(HEARTBEAT_INTERVAL * 2 < IDLE_TIMEOUT);
    }

    #[test]
    fn default_state_uses_production_timings() {
        // Тестовый конструктор существует ради коротких таймаутов — но
        // обычный путь обязан остаться боевым.
        let st = crate::state::VkState::new(crate::config::VkConfig::default());
        assert_eq!(st.ws_heartbeat, HEARTBEAT_INTERVAL);
        assert_eq!(st.ws_idle_timeout, IDLE_TIMEOUT);
    }

    #[test]
    fn query_token_is_optional() {
        let q: VkWsQuery = serde_json::from_str("{}").unwrap();
        assert!(q.token.is_none());
        let q: VkWsQuery = serde_json::from_str(r#"{"token":"t"}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some("t"));
    }

    #[test]
    fn query_tolerates_unknown_params_and_empty_token() {
        // Новый фронт может дописать параметры в URL реконнекта (версия
        // клиента, id вкладки) — старый сервер обязан их игнорировать, а не
        // отвечать 400 на апгрейд. Тест сломается, если на структуру навесят
        // deny_unknown_fields.
        let q: VkWsQuery = serde_json::from_str(r#"{"token":"t","v":"2","tab":"7"}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some("t"));

        // `?token=` (пустое значение) — реальный URL при незаполненном
        // сторадже фронта: это Some(""), и апгрейд от этого падать не должен.
        let q: VkWsQuery = serde_json::from_str(r#"{"token":""}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some(""));
    }
}
