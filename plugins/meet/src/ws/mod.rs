//! WebSocket-handler `/ws/meet?token=`.
//!
//! Цикл — **построчная копия** `plugins/telegram/src/ws/mod.rs`. Это не
//! экономия мысли: каждое из перечисленных ниже свойств в telegram уже
//! вылечено после конкретного бага, и отклонение здесь воспроизвело бы его
//! заново.
//!
//! ## Жизненный цикл соединения
//!
//! 1. [`meet_ws`] апгрейдит HTTP в WebSocket. Bearer-auth (в remote-mode) уже
//!    выполнен middleware'ом хоста — параметр `token` в query существует
//!    только потому, что браузерный `WebSocket` не умеет слать заголовок
//!    `Authorization`.
//! 2. [`handle_socket`] **подписывается на broadcast ДО отправки снапшота**.
//!    Порядок обязателен: событие, случившееся между снимком и подпиской,
//!    иначе потерялось бы навсегда — и запись, начатая в этот момент, не
//!    появилась бы в интерфейсе до перезагрузки страницы.
//! 3. Первый кадр — [`MeetServerMsg::Snapshot`]: активные встречи, состояние
//!    зависимостей и лимиты. Клиенту не нужно ждать события, чтобы отрисовать
//!    индикатор записи.
//! 4. Дальше один `tokio::select!` с `biased`: порядок веток детерминирован,
//!    и сигнал остановки сервера обрабатывается раньше всего остального.
//! 5. `RecvError::Lagged(n)` НЕ рвёт соединение — уходит
//!    [`MeetServerMsg::Resync`]. Отставание случается ровно в момент пиковой
//!    нагрузки, и лишний handshake нужен там меньше всего.
//!
//! ## Фильтрации по конкретной встрече НЕТ
//!
//! Индикатор записи обязан работать с ЛЮБОЙ вкладки, а не только с той, где
//! встречу запустили: человек нажал кнопку в Telegram, ушёл в терминал — и
//! запись должна быть видна ему и там. Поэтому сервер вещает всё, а решает,
//! что показать, фронт.
//!
//! ## Тайминги берутся из состояния
//!
//! В бою они равны [`HEARTBEAT_INTERVAL`] и [`IDLE_TIMEOUT`], а тест поднимает
//! состояние с сотнями миллисекунд и проверяет НАСТОЯЩИЙ цикл, не дожидаясь
//! боевой минуты (см. `MeetState::with_ws_timings`).

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

use crate::state::MeetState;
use crate::status::MeetStatus;
use crate::ws::protocol::{MeetClientMsg, MeetServerMsg};

/// Период heartbeat-пинга. Совпадает с остальными плагинами (15 с):
/// realtime-вкладка должна замечать разрыв быстро.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Тишина дольше этого — соединение мёртвое. Четыре пропущенных пинга подряд.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Query `/ws/meet?token=<bearer>`.
#[derive(Debug, Default, Deserialize)]
pub struct MeetWsQuery {
    /// Проверку делает middleware хоста; здесь параметр только принимается.
    #[serde(default)]
    pub token: Option<String>,
}

/// HTTP-handler апгрейда в WebSocket.
pub async fn meet_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<MeetState>>,
    Query(_q): Query<MeetWsQuery>,
) -> Response {
    tracing::debug!(target: "forge_meet", "meet_ws: upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

/// Собирает первый кадр соединения.
///
/// Только АКТИВНЫЕ встречи: у пользователя могут быть тысячи записанных, и
/// слать их целиком на каждый открытый сокет означало бы мегабайты. Состояния
/// завершённых приезжают батчем ленты.
///
/// Ошибка базы не мешает снапшоту: он обязан уйти в любом случае, потому что
/// несёт ещё и `deps` с `limits` — то есть единственную диагностику, доступную
/// фронту в момент, когда что-то сломано.
pub async fn build_snapshot(state: &Arc<MeetState>) -> MeetServerMsg {
    let meetings = match crate::db::repo::meetings::list_by_status(state, &MeetStatus::ACTIVE).await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                error = %e,
                "forge-meet: снапшот без списка встреч"
            );
            Vec::new()
        }
    };
    MeetServerMsg::Snapshot {
        meetings,
        deps: state.deps_snapshot().await,
        limits: state.limits(),
    }
}

/// Главный цикл одного WS-соединения.
async fn handle_socket(socket: WebSocket, state: Arc<MeetState>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(Mutex::new(ws_tx));

    // ПОДПИСКА ДО СНАПШОТА. Обратный порядок терял бы события, случившиеся в
    // зазоре между снимком и подпиской.
    let mut broadcast_rx = state.subscribe();

    let snapshot = build_snapshot(&state).await;
    if send_server_msg(&ws_tx, &snapshot).await.is_err() {
        return;
    }

    let mut heartbeat = tokio::time::interval(state.ws_heartbeat);
    heartbeat.tick().await; // первый тик мгновенный — пропускаем.
    let mut last_activity = Instant::now();

    loop {
        let idle_deadline = last_activity + state.ws_idle_timeout;
        tokio::select! {
            biased;

            // Остановка сервера — закрываемся первыми.
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_meet", "meet_ws: shutdown, closing");
                break;
            }

            _ = tokio::time::sleep_until(idle_deadline) => {
                tracing::info!(target: "forge_meet", "meet_ws: idle timeout, closing");
                break;
            }

            _ = heartbeat.tick() => {
                if send_server_msg(&ws_tx, &MeetServerMsg::Ping).await.is_err() {
                    break;
                }
            }

            // Событие из broadcast — БЕЗ фильтрации.
            ev = broadcast_rx.recv() => {
                match ev {
                    Ok(msg) => {
                        if send_server_msg(&ws_tx, &msg).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            target: "forge_meet",
                            skipped = n,
                            "meet_ws: broadcast lagged, sending resync"
                        );
                        if send_server_msg(&ws_tx, &MeetServerMsg::Resync).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(target: "forge_meet", "meet_ws: broadcast closed");
                        break;
                    }
                }
            }

            opt = ws_rx.next() => {
                match opt {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!(target: "forge_meet", "meet_ws: client closed");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_activity = Instant::now();
                        match serde_json::from_str::<MeetClientMsg>(&text) {
                            Ok(MeetClientMsg::Pong) => {}
                            Err(e) => {
                                // Не рвём соединение: незнакомый кадр может
                                // прийти от более новой версии фронта.
                                tracing::warn!(
                                    target: "forge_meet",
                                    error = %e,
                                    raw = %text,
                                    "meet_ws: malformed client frame, ignored"
                                );
                            }
                        }
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(_)) => {
                        last_activity = Instant::now();
                    }
                    Some(Err(e)) => {
                        tracing::debug!(target: "forge_meet", error = ?e, "meet_ws: recv error");
                        break;
                    }
                }
            }
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::debug!(target: "forge_meet", "meet_ws: terminated");
}

/// Отправляет кадр текстовым сообщением.
///
/// Ошибка сериализации не рвёт соединение — это баг сервера, а не клиента.
async fn send_server_msg(ws_tx: &WsSink, msg: &MeetServerMsg) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "forge_meet", error = %e, "meet_ws: serialize failed");
            return Ok(());
        }
    };
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeetConfig;

    fn state() -> Arc<MeetState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            ..MeetConfig::default()
        };
        std::mem::forget(dir);
        MeetState::new(cfg)
    }

    #[test]
    fn heartbeat_is_shorter_than_idle_timeout() {
        // Иначе соединение умирало бы раньше, чем клиент успевал ответить.
        assert!(HEARTBEAT_INTERVAL * 2 < IDLE_TIMEOUT);
    }

    #[test]
    fn the_default_state_uses_production_timings() {
        let st = state();
        assert_eq!(st.ws_heartbeat, HEARTBEAT_INTERVAL);
        assert_eq!(st.ws_idle_timeout, IDLE_TIMEOUT);
    }

    #[test]
    fn query_token_is_optional_and_unknown_params_are_tolerated() {
        let q: MeetWsQuery = serde_json::from_str("{}").unwrap();
        assert!(q.token.is_none());
        // Новый фронт может дописать параметры в URL реконнекта — старый
        // сервер обязан их игнорировать, а не отвечать 400 на апгрейд.
        let q: MeetWsQuery = serde_json::from_str(r#"{"token":"t","v":"2"}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some("t"));
        let q: MeetWsQuery = serde_json::from_str(r#"{"token":""}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some(""));
    }

    /// Первый кадр — снапшот, даже когда база не открыта.
    #[tokio::test]
    async fn the_first_frame_is_always_a_snapshot() {
        let st = state();
        let snap = build_snapshot(&st).await;
        match snap {
            MeetServerMsg::Snapshot { meetings, limits, .. } => {
                assert!(meetings.is_empty());
                assert_eq!(limits.whisper, 1);
            }
            other => panic!("первым кадром обязан быть снапшот, а не {other:?}"),
        }
    }

    /// Переполнение канала даёт `Resync`, а не разрыв. Проверяется на
    /// настоящем broadcast'е состояния.
    #[tokio::test]
    async fn an_overflowing_channel_yields_resync_not_a_disconnect() {
        let st = state();
        let mut rx = st.subscribe();
        for _ in 0..(crate::state::BROADCAST_CAPACITY + 10) {
            st.broadcast_event(MeetServerMsg::Ping);
        }
        let err = rx.recv().await.expect_err("канал обязан сообщить об отставании");
        assert!(matches!(
            err,
            tokio::sync::broadcast::error::RecvError::Lagged(_)
        ));

        // Приёмник ЖИВ — именно на этом стоит решение слать Resync вместо
        // разрыва. Дочитываем накопленное (повторный Lagged здесь законен:
        // буфер всё ещё полон) и убеждаемся, что НОВЫЙ кадр доходит.
        use tokio::sync::broadcast::error::TryRecvError;
        loop {
            match rx.try_recv() {
                Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => panic!("канал закрылся — соединение было бы разорвано"),
            }
        }
        st.broadcast_event(MeetServerMsg::Resync);
        assert_eq!(rx.recv().await.unwrap(), MeetServerMsg::Resync);
    }

    /// События встречи, начатой «в другой сессии», доходят до этого сокета:
    /// фильтрации по встрече нет.
    #[tokio::test]
    async fn events_from_another_session_reach_every_subscriber() {
        let st = state();
        let mut a = st.subscribe();
        let mut b = st.subscribe();
        st.broadcast_event(MeetServerMsg::NeedsHuman {
            meeting_id: "чужая-встреча".into(),
            kind: "captcha".into(),
        });
        for rx in [&mut a, &mut b] {
            match rx.recv().await.unwrap() {
                MeetServerMsg::NeedsHuman { meeting_id, .. } => {
                    assert_eq!(meeting_id, "чужая-встреча");
                }
                other => panic!("не тот кадр: {other:?}"),
            }
        }
    }

    /// Подписка ДО снапшота: если бы порядок был обратным, событие из зазора
    /// потерялось бы. Тест воспроизводит именно зазор.
    #[tokio::test]
    async fn subscribing_before_the_snapshot_loses_nothing() {
        let st = state();
        let mut rx = st.subscribe();
        // «Зазор»: событие случилось, пока строился снапшот.
        st.broadcast_event(MeetServerMsg::Ping);
        let _snapshot = build_snapshot(&st).await;
        assert_eq!(
            rx.recv().await.unwrap(),
            MeetServerMsg::Ping,
            "событие из зазора обязано дойти"
        );
    }
}
