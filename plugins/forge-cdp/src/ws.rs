//! Боевая реализация [`CdpTransport`] поверх `tokio-tungstenite`.
//!
//! # Устройство
//!
//! Одно соединение, две задачи и роутер:
//!
//! * **писатель** — забирает готовые кадры из очереди и пишет в сокет;
//! * **читатель** — разбирает входящие кадры и раздаёт их: ответ уходит в
//!   `oneshot`-канал ждущей команды (поиск по `id`), событие —
//!   в broadcast всем подписчикам.
//!
//! Разделение на две задачи, а не «пишем прямо из `send`», нужно ради
//! конкурентности: команды летят одновременно из адаптера платформы, из насоса
//! автоаттача и из приёмника кадров, и сериализовать их в общий сокет должен
//! кто-то один.
//!
//! # Почему ответы ищутся по `id`, а не по порядку
//!
//! CDP не гарантирует порядок ответов: медленный `Page.navigate` вернётся
//! позже быстрого `Runtime.evaluate`, отправленного после него. Очередь
//! «первый отправленный — первый отвеченный» склеила бы чужие ответы, и
//! проявилось бы это не падением, а тихо неверными данными.
//!
//! # Закрытие будит всех
//!
//! Когда сокет умирает (Chrome убит, упал, отвалился), карта ждущих команд
//! очищается: `oneshot`-отправители дропаются, и каждая ждущая команда
//! немедленно получает [`CdpError::TransportClosed`]. Иначе запись встречи
//! зависла бы навсегда на команде, ответа на которую уже не будет.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{CdpCommand, CdpError, CdpEvent, CdpMessage, ProtocolError};
use crate::transport::{CdpTransport, TransportConfig};

type PendingSlot = oneshot::Sender<Result<Value, ProtocolError>>;

struct Inner {
    out_tx: mpsc::UnboundedSender<Message>,
    pending: Mutex<HashMap<u64, PendingSlot>>,
    events: broadcast::Sender<CdpEvent>,
    next_id: AtomicU64,
    closed: AtomicBool,
    config: TransportConfig,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Inner {
    fn dispatch(&self, raw: &str) {
        match CdpMessage::parse(raw) {
            Ok(CdpMessage::Response(resp)) => {
                let slot = self.pending.lock().unwrap().remove(&resp.id);
                match slot {
                    Some(tx) => {
                        // Ошибка отправки означает, что ждавший уже ушёл по
                        // таймауту. Это нормально и не повод шуметь.
                        let _ = tx.send(resp.result);
                    }
                    None => tracing::debug!(
                        id = resp.id,
                        "forge-cdp: ответ на команду, которую уже никто не ждёт"
                    ),
                }
            }
            Ok(CdpMessage::Event(event)) => {
                // Ошибка = подписчиков нет. Это штатно: события идут всегда,
                // а слушают их не всегда.
                let _ = self.events.send(event);
            }
            Err(e) => tracing::warn!(error = %e, "forge-cdp: кадр не разобран"),
        }
    }

    /// Помечает транспорт закрытым и будит всех ждущих.
    fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let drained: Vec<PendingSlot> = {
            let mut pending = self.pending.lock().unwrap();
            pending.drain().map(|(_, tx)| tx).collect()
        };
        let count = drained.len();
        drop(drained); // дроп отправителей = мгновенное пробуждение ждущих
        if count > 0 {
            tracing::warn!(
                count,
                "forge-cdp: транспорт закрыт, ждущие команды разбужены ошибкой"
            );
        }
    }
}

/// Транспорт CDP поверх WebSocket.
pub struct WsTransport {
    inner: Arc<Inner>,
}

impl WsTransport {
    /// Подключается к ws-эндпоинту браузера (адрес из `DevToolsActivePort`).
    ///
    /// Возвращает `Arc`, потому что транспорт всегда разделяемый: им владеют
    /// сессии, насос автоаттача и приёмник биндингов одновременно.
    pub async fn connect(url: &str, config: TransportConfig) -> Result<Arc<Self>, CdpError> {
        let (stream, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| CdpError::Ws(format!("connect_async({url}): {e}")))?;
        let (mut write, mut read) = stream.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let (events, _) = broadcast::channel(config.event_buffer);

        let inner = Arc::new(Inner {
            out_tx,
            pending: Mutex::new(HashMap::new()),
            events,
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            config,
            tasks: Mutex::new(Vec::new()),
        });

        let writer_inner = inner.clone();
        let writer = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if let Err(e) = write.send(msg).await {
                    tracing::warn!(error = %e, "forge-cdp: запись в сокет CDP не удалась");
                    break;
                }
            }
            let _ = write.close().await;
            writer_inner.mark_closed();
        });

        let reader_inner = inner.clone();
        let reader = tokio::spawn(async move {
            while let Some(frame) = read.next().await {
                match frame {
                    Ok(Message::Text(txt)) => reader_inner.dispatch(&txt),
                    // CDP не шлёт бинарных кадров; молча игнорировать нельзя —
                    // это признак, что мы говорим не с тем сервером.
                    Ok(Message::Binary(b)) => tracing::warn!(
                        len = b.len(),
                        "forge-cdp: бинарный кадр в протоколе, где его быть не должно"
                    ),
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "forge-cdp: обрыв сокета CDP");
                        break;
                    }
                }
            }
            reader_inner.mark_closed();
        });

        inner.tasks.lock().unwrap().extend([writer, reader]);
        Ok(Arc::new(Self { inner }))
    }

    /// Настройки, с которыми транспорт создан.
    pub fn config(&self) -> &TransportConfig {
        &self.inner.config
    }
}

#[async_trait]
impl CdpTransport for WsTransport {
    async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(CdpError::TransportClosed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let cmd = CdpCommand::new(id, method, params, session_id.map(str::to_owned));

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);

        if self
            .inner
            .out_tx
            .send(Message::Text(cmd.to_json_string()))
            .is_err()
        {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(CdpError::TransportClosed);
        }

        match tokio::time::timeout(self.inner.config.command_timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(CdpError::Protocol {
                method: method.to_string(),
                error,
            }),
            // Отправитель дропнут: транспорт закрылся, пока команда летела.
            Ok(Err(_)) => Err(CdpError::TransportClosed),
            Err(_) => {
                // Слот обязательно убрать: иначе карта ждущих растёт на
                // каждом таймауте до конца встречи.
                self.inner.pending.lock().unwrap().remove(&id);
                Err(CdpError::Timeout {
                    method: method.to_string(),
                    timeout_ms: self.inner.config.command_timeout.as_millis() as u64,
                })
            }
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    async fn close(&self) {
        self.inner.mark_closed();
        let tasks: Vec<JoinHandle<()>> = self.inner.tasks.lock().unwrap().drain(..).collect();
        for t in tasks {
            t.abort();
        }
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        self.inner.mark_closed();
        let tasks: Vec<JoinHandle<()>> = self.inner.tasks.lock().unwrap().drain(..).collect();
        for t in tasks {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::EventFilter;
    use crate::testing::FakeCdpServer;
    use crate::transport::wait_for_event;
    use serde_json::json;
    use std::time::Duration;

    /// Двадцать команд разом: каждая обязана получить СВОЙ ответ.
    ///
    /// Сервер отвечает в ОБРАТНОМ порядке и с разной задержкой — если бы
    /// роутинг шёл по порядку отправки, тест бы это поймал.
    #[tokio::test]
    async fn parallel_commands_do_not_mix_up_responses() {
        let server = FakeCdpServer::start(|req| {
            let id = req["id"].as_u64().unwrap();
            Some(json!({"id": id, "result": {"echo": id * 100}}))
        })
        .await;
        let transport = WsTransport::connect(&server.url, TransportConfig::default())
            .await
            .unwrap();

        let mut handles = Vec::new();
        for _ in 0..20 {
            let t = transport.clone();
            handles.push(tokio::spawn(async move {
                t.send("Runtime.evaluate", json!({}), None).await
            }));
        }
        let mut echoes = Vec::new();
        for h in handles {
            echoes.push(h.await.unwrap().unwrap()["echo"].as_u64().unwrap());
        }
        echoes.sort_unstable();
        assert_eq!(
            echoes,
            (1..=20).map(|i| i * 100).collect::<Vec<_>>(),
            "каждая команда должна получить ответ именно на свой id"
        );
    }

    #[tokio::test]
    async fn protocol_error_carries_code() {
        let server = FakeCdpServer::start(|req| {
            let id = req["id"].as_u64().unwrap();
            Some(json!({"id": id, "error": {"code": -32000, "message": "Cannot find context"}}))
        })
        .await;
        let transport = WsTransport::connect(&server.url, TransportConfig::default())
            .await
            .unwrap();
        let err = transport
            .send("Runtime.evaluate", json!({}), Some("S1"))
            .await
            .unwrap_err();
        assert_eq!(err.protocol_code(), Some(-32000));
    }

    /// События дочерней цели (со своим `sessionId`) доходят до подписчика.
    #[tokio::test]
    async fn events_from_child_session_reach_subscriber() {
        let server = FakeCdpServer::start(|_req| None).await;
        let transport = WsTransport::connect(&server.url, TransportConfig::default())
            .await
            .unwrap();
        // Подписка ДО того, как событие послано: иначе есть окно потери.
        let mut rx = transport.subscribe();

        server.push(json!({
            "method": "Runtime.bindingCalled",
            "sessionId": "CHILD-1",
            "params": {"name": "__forge_ab", "payload": "{\"t\":1}"}
        }));

        let event = wait_for_event(
            &mut rx,
            &EventFilter::method("Runtime.bindingCalled").in_session("CHILD-1"),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(event.params["name"], "__forge_ab");
        assert_eq!(event.session_id.as_deref(), Some("CHILD-1"));
    }

    /// Молчащий сервер даёт таймаут, а не вечное ожидание.
    #[tokio::test]
    async fn silent_server_yields_timeout_not_hang() {
        let server = FakeCdpServer::start(|_req| None).await;
        let cfg = TransportConfig {
            command_timeout: Duration::from_millis(200),
            ..TransportConfig::default()
        };
        let transport = WsTransport::connect(&server.url, cfg).await.unwrap();
        let err = transport.send("Page.enable", json!({}), None).await.unwrap_err();
        match err {
            CdpError::Timeout { method, timeout_ms } => {
                assert_eq!(method, "Page.enable");
                assert_eq!(timeout_ms, 200);
            }
            other => panic!("ожидался таймаут, получено {other:?}"),
        }
        assert!(!transport.is_closed(), "таймаут команды не закрывает транспорт");
    }

    /// Закрытие сокета будит ВСЕХ ждущих, а не одного.
    #[tokio::test]
    async fn socket_close_wakes_every_pending_command() {
        let server = FakeCdpServer::start(|_req| None).await;
        let cfg = TransportConfig {
            command_timeout: Duration::from_secs(30),
            ..TransportConfig::default()
        };
        let transport = WsTransport::connect(&server.url, cfg).await.unwrap();

        let mut handles = Vec::new();
        for _ in 0..5 {
            let t = transport.clone();
            handles.push(tokio::spawn(async move {
                t.send("Page.navigate", json!({}), None).await
            }));
        }
        // Даём командам уйти в ожидание.
        tokio::time::sleep(Duration::from_millis(100)).await;
        server.shutdown();

        for h in handles {
            let err = h.await.unwrap().unwrap_err();
            assert!(
                matches!(err, CdpError::TransportClosed),
                "ждущая команда должна проснуться TransportClosed, получено {err:?}"
            );
        }
        assert!(transport.is_closed());
        // После закрытия новые команды сразу отбиваются.
        let err = transport.send("Page.enable", json!({}), None).await.unwrap_err();
        assert!(matches!(err, CdpError::TransportClosed));
    }

    #[tokio::test]
    async fn command_wire_carries_session_id() {
        let server = FakeCdpServer::start(|req| {
            let id = req["id"].as_u64().unwrap();
            Some(json!({"id": id, "result": {}}))
        })
        .await;
        let transport = WsTransport::connect(&server.url, TransportConfig::default())
            .await
            .unwrap();
        transport
            .send("Page.enable", json!({}), Some("SESSION-7"))
            .await
            .unwrap();
        let seen = server.received();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["sessionId"], "SESSION-7");
        assert_eq!(seen[0]["method"], "Page.enable");
    }
}
