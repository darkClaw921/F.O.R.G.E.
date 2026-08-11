//! WebSocket-handler `/ws/bitrix?token=`.
//!
//! ## Жизненный цикл соединения
//!
//! 1. [`bitrix_ws`] апгрейдит HTTP в WebSocket. Bearer-auth (в remote-mode)
//!    уже выполнен middleware'ом хоста — query-параметр `token` существует
//!    только потому, что браузерный `WebSocket` не умеет слать заголовок
//!    `Authorization`.
//! 2. [`handle_socket`] сразу подписывается на broadcast и **первым кадром**
//!    отправляет текущий `login_state`: клиенту не нужно ждать события, чтобы
//!    отрисовать экран подключения.
//! 3. Дальше — один `tokio::select!`:
//!    - событие из broadcast → кадр клиенту (БЕЗ фильтрации, см. ниже);
//!    - входящий кадр → парсим [`B24ClientMsg`], `Pong` сбрасывает
//!      idle-таймер;
//!    - тик heartbeat'а (15 с) → [`B24ServerMsg::Ping`];
//!    - тишина [`IDLE_TIMEOUT`] (60 с) → закрываем соединение;
//!    - `shutdown`-токен → закрываем соединение.
//! 4. `RecvError::Lagged(n)` НЕ рвёт соединение: клиенту уходит
//!    [`B24ServerMsg::Resync`], он перечитывает состояние по REST (Р19).
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
//! Бейдж непрочитанного на кнопке вкладки и тосты о новых сообщениях должны
//! работать, когда открыта ЛЮБАЯ вкладка devforge. Поэтому сервер вещает все
//! события, а решает, что показать, фронтенд (в том числе применяя белый
//! список чатов, Ф11).
//!
//! Число живых подписчиков — не только статистика: воркер событий (Ф7)
//! выбирает по нему каденс опроса портала (`ws_subscribers() == 0` ⇒
//! idle-интервал, Р4).

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

use crate::state::B24State;
use crate::ws::protocol::{B24ClientMsg, B24ServerMsg};

/// Период heartbeat-пинга. Совпадает с Echo, Telegram и VK (15 с):
/// realtime-вкладка должна замечать разрыв быстро, чтобы фронт успел
/// переподключиться.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Если 60 с не пришло ни одного кадра (включая `pong`) — соединение
/// считается мёртвым и закрывается. Четыре пропущенных пинга подряд.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Query `/ws/bitrix?token=<bearer>`.
#[derive(Debug, Default, Deserialize)]
pub struct B24WsQuery {
    /// Опциональный bearer для браузерного WebSocket. Проверку делает
    /// middleware хоста, здесь параметр только принимается и игнорируется.
    #[serde(default)]
    pub token: Option<String>,
}

/// HTTP-handler апгрейда в WebSocket.
pub async fn bitrix_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<B24State>>,
    Query(_q): Query<B24WsQuery>,
) -> Response {
    tracing::debug!(target: "forge_bitrix", "bitrix_ws: upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Тип отправляющей половины сокета под общим Mutex'ом.
type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

/// Главный цикл одного WS-соединения.
async fn handle_socket(socket: WebSocket, state: Arc<B24State>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(Mutex::new(ws_tx));

    // Подписка ДО отправки снапшота: событие, случившееся между снапшотом и
    // подпиской, иначе потерялось бы навсегда.
    let mut broadcast_rx = state.subscribe();

    let snapshot = B24ServerMsg::LoginState(state.login_state_dto().await);
    if send_server_msg(&ws_tx, &snapshot).await.is_err() {
        return;
    }

    // Тайминги берутся из состояния, а не из констант: в проде они равны
    // HEARTBEAT_INTERVAL/IDLE_TIMEOUT, а интеграционный тест поднимает
    // состояние с сотнями миллисекунд и проверяет настоящий цикл, не дожидаясь
    // боевой минуты (см. B24State::with_ws_timings).
    let mut heartbeat = tokio::time::interval(state.ws_heartbeat);
    heartbeat.tick().await; // первый тик мгновенный — пропускаем.
    let mut last_activity = Instant::now();

    loop {
        let idle_deadline = last_activity + state.ws_idle_timeout;
        tokio::select! {
            biased;

            // Остановка сервера — закрываемся первыми.
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_bitrix", "bitrix_ws: shutdown, closing");
                break;
            }

            // Idle-таймаут.
            _ = tokio::time::sleep_until(idle_deadline) => {
                tracing::info!(target: "forge_bitrix", "bitrix_ws: idle timeout, closing");
                break;
            }

            // Heartbeat.
            _ = heartbeat.tick() => {
                if send_server_msg(&ws_tx, &B24ServerMsg::Ping).await.is_err() {
                    break;
                }
            }

            // Событие из broadcast — без фильтрации.
            ev = broadcast_rx.recv() => {
                match broadcast_frame(ev) {
                    Some(msg) => {
                        if send_server_msg(&ws_tx, &msg).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }

            // Входящий кадр.
            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!(target: "forge_bitrix", "bitrix_ws: client closed");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_activity = Instant::now();
                        match serde_json::from_str::<B24ClientMsg>(&text) {
                            Ok(B24ClientMsg::Pong) => {}
                            Err(e) => {
                                // Не рвём соединение: неизвестный кадр может
                                // прийти от более новой версии фронта.
                                tracing::warn!(
                                    target: "forge_bitrix",
                                    error = %e,
                                    "bitrix_ws: malformed client frame, ignored"
                                );
                            }
                        }
                    }
                    // Протокольные ping/pong тоже считаем активностью.
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(_)) => {
                        last_activity = Instant::now();
                    }
                    Some(Err(e)) => {
                        tracing::debug!(target: "forge_bitrix", error = ?e, "bitrix_ws: recv error");
                        break;
                    }
                }
            }
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::debug!(target: "forge_bitrix", "bitrix_ws: terminated");
}

/// Что отправить клиенту по результату чтения broadcast-канала.
///
/// `None` означает «закрываем соединение», `Some(msg)` — кадр к отправке.
///
/// # Р19: `Lagged` → `resync`, и НИКОГДА не досылка
///
/// Отставший подписчик (канал ёмкостью [`crate::state::BROADCAST_CAPACITY`]
/// переполнился) пропустил `n` кадров, и восстановить их неоткуда: broadcast
/// хранит только хвост. Единственный честный ответ — сказать клиенту
/// «перечитай состояние по REST»; попытка «доcлать пропущенное» дала бы ленту,
/// собранную из дырявой последовательности, причём молча.
///
/// Отдельная функция, а не ветка `match` внутри цикла, по одной причине:
/// правило проверяется тестом. Внутри `handle_socket` его пришлось бы
/// проверять поднятым сокетом с настоящим апгрейдом — то есть не проверять
/// вовсе.
pub fn broadcast_frame(
    ev: Result<B24ServerMsg, tokio::sync::broadcast::error::RecvError>,
) -> Option<B24ServerMsg> {
    match ev {
        Ok(msg) => Some(msg),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                target: "forge_bitrix",
                skipped = n,
                "bitrix_ws: broadcast lagged, sending resync"
            );
            Some(B24ServerMsg::Resync)
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            tracing::debug!(target: "forge_bitrix", "bitrix_ws: broadcast closed");
            None
        }
    }
}

/// Отправляет [`B24ServerMsg`] текстовым кадром.
///
/// Ошибка сериализации не должна рвать соединение (это баг сервера, а не
/// клиента), поэтому логируется и трактуется как успех.
async fn send_server_msg(ws_tx: &WsSink, msg: &B24ServerMsg) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "forge_bitrix", error = %e, "bitrix_ws: serialize failed");
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
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        assert_eq!(st.ws_heartbeat, HEARTBEAT_INTERVAL);
        assert_eq!(st.ws_idle_timeout, IDLE_TIMEOUT);
    }

    #[test]
    fn query_token_is_optional_and_tolerates_extras() {
        let q: B24WsQuery = serde_json::from_str("{}").unwrap();
        assert!(q.token.is_none());
        // Новый фронт может дописать параметры в URL реконнекта (версия
        // клиента, id вкладки) — старый сервер обязан их игнорировать, а не
        // отвечать 400 на апгрейд.
        let q: B24WsQuery = serde_json::from_str(r#"{"token":"t","v":"2"}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some("t"));
        // `?token=` (пустое значение) — реальный URL при незаполненном
        // сторадже фронта.
        let q: B24WsQuery = serde_json::from_str(r#"{"token":""}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some(""));
    }

    /// Переполнение broadcast обязано превращаться в `Lagged`, а не в тихую
    /// потерю событий: именно на этом держится кадр `resync` (Р19).
    #[tokio::test]
    async fn broadcast_overflow_reports_lagged() {
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        let mut rx = st.subscribe();
        for _ in 0..(crate::state::BROADCAST_CAPACITY + 8) {
            st.broadcast_event(B24ServerMsg::Ping);
        }
        match rx.recv().await {
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => assert!(n > 0),
            other => panic!("ожидался Lagged, пришло {other:?}"),
        }
    }

    /// Р19 целиком, от переполнения канала до кадра: отставший подписчик
    /// получает `resync` и НЕ получает досылки пропущенного.
    #[tokio::test]
    async fn a_lagging_subscriber_gets_a_resync_frame() {
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        let mut rx = st.subscribe();
        // Заливаем канал заведомо сильнее ёмкости — ровно то, что делает
        // активный портал с вкладкой, которую усыпил браузер.
        for _ in 0..(crate::state::BROADCAST_CAPACITY + 8) {
            st.broadcast_event(B24ServerMsg::Ping);
        }

        assert_eq!(
            broadcast_frame(rx.recv().await),
            Some(B24ServerMsg::Resync),
            "отставший подписчик обязан получить resync"
        );
        // Дальше поток продолжается с ЖИВОГО хвоста, а не с пропущенного
        // начала: досылки нет и быть не может — broadcast хранит только хвост.
        assert_eq!(
            broadcast_frame(rx.recv().await),
            Some(B24ServerMsg::Ping)
        );

        // Обычное событие проходит как есть.
        st.broadcast_event(B24ServerMsg::Resync);
        // Закрытый канал — это конец соединения, а не кадр.
        drop(st);
        let mut drained = rx;
        while let Ok(msg) = drained.try_recv() {
            assert!(matches!(msg, B24ServerMsg::Ping | B24ServerMsg::Resync));
        }
        assert_eq!(broadcast_frame(drained.recv().await), None);
    }
}
