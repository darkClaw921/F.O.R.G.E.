//! Помощники для тестов: фейковый ws-сервер и записывающий транспорт.
//!
//! Модуль виден снаружи только при feature `testing` — тестовый код не должен
//! попадать в бинарь devforge. Внутри крейта он доступен всегда под
//! `cfg(test)`.
//!
//! Два инструмента отвечают на два разных вопроса:
//!
//! * [`FakeCdpServer`] — «правильно ли устроен ТРАНСПОРТ»: настоящий
//!   WebSocket, настоящие кадры, настоящий обрыв соединения, но без Chrome.
//! * [`RecordingTransport`] — «в каком порядке и с какими параметрами ушли
//!   КОМАНДЫ»: сокета нет вовсе, зато есть точный список вызовов. Именно им
//!   проверяется порядок `setBypassCSP` → … → `navigate`, нарушение которого
//!   ломает инжект молча.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{CdpError, CdpEvent, ProtocolError};
use crate::transport::CdpTransport;

/// Фейковый CDP-сервер: настоящий WebSocket на локальном порту.
///
/// Отвечает на команды функцией-обработчиком (может не отвечать вовсе — так
/// проверяется таймаут), умеет ПУШИТЬ события клиенту и умеет обрывать
/// соединение — три вещи, которые нужны, чтобы проверить транспорт без
/// живого браузера.
pub struct FakeCdpServer {
    /// Адрес для [`crate::ws::WsTransport::connect`].
    pub url: String,
    push_tx: mpsc::UnboundedSender<Value>,
    received: Arc<Mutex<Vec<Value>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl FakeCdpServer {
    /// Поднимает сервер на свободном порту.
    ///
    /// `handler` получает разобранную команду и возвращает кадр ответа либо
    /// `None`, если отвечать не нужно.
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&Value) -> Option<Value> + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}/devtools/browser/fake");
        let (push_tx, mut push_rx) = mpsc::unbounded_channel::<Value>();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_task = received.clone();

        let task = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (mut write, mut read) = ws.split();
            loop {
                tokio::select! {
                    frame = read.next() => match frame {
                        Some(Ok(Message::Text(txt))) => {
                            let Ok(value) = serde_json::from_str::<Value>(&txt) else { continue };
                            received_task.lock().unwrap().push(value.clone());
                            if let Some(resp) = handler(&value) {
                                if write.send(Message::Text(resp.to_string())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break,
                    },
                    pushed = push_rx.recv() => match pushed {
                        Some(frame) => {
                            if write.send(Message::Text(frame.to_string())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        });

        Self {
            url,
            push_tx,
            received,
            task: Mutex::new(Some(task)),
        }
    }

    /// Отправляет клиенту произвольный кадр (обычно событие).
    pub fn push(&self, frame: Value) {
        let _ = self.push_tx.send(frame);
    }

    /// Все команды, полученные сервером, в порядке прихода.
    pub fn received(&self) -> Vec<Value> {
        self.received.lock().unwrap().clone()
    }

    /// Методы полученных команд — короткая форма для проверки порядка.
    pub fn received_methods(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter_map(|v| v["method"].as_str().map(str::to_owned))
            .collect()
    }

    /// Рвёт соединение: проверка «закрытие будит всех ждущих».
    pub fn shutdown(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }
}

impl Drop for FakeCdpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Один записанный вызов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// Имя метода CDP.
    pub method: String,
    /// Параметры, как их отправил вызывающий код.
    pub params: Value,
    /// Адресат.
    pub session_id: Option<String>,
}

/// Транспорт без сокета: записывает вызовы и отдаёт заготовленные ответы.
pub struct RecordingTransport {
    calls: Mutex<Vec<RecordedCall>>,
    responses: Mutex<HashMap<String, Value>>,
    failures: Mutex<HashMap<String, ProtocolError>>,
    events: broadcast::Sender<CdpEvent>,
    closed: AtomicBool,
}

impl RecordingTransport {
    /// Новый транспорт с разумными ответами по умолчанию.
    ///
    /// Умолчания подобраны так, чтобы тест, проверяющий ПОРЯДОК команд, не
    /// был обязан описывать ответ на каждую из них.
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let mut responses = HashMap::new();
        responses.insert(
            "Target.createTarget".to_string(),
            json!({"targetId": "TARGET-FAKE"}),
        );
        responses.insert(
            "Target.attachToTarget".to_string(),
            json!({"sessionId": "SESSION-FAKE"}),
        );
        responses.insert(
            "Runtime.evaluate".to_string(),
            json!({"result": {"type": "undefined"}}),
        );
        responses.insert(
            "Browser.getWindowForTarget".to_string(),
            json!({"windowId": 1, "bounds": {"left": 0, "top": 0, "width": 1280, "height": 800}}),
        );
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
            failures: Mutex::new(HashMap::new()),
            events,
            closed: AtomicBool::new(false),
        })
    }

    /// Задать ответ на метод.
    pub fn set_response(&self, method: &str, result: Value) {
        self.responses.lock().unwrap().insert(method.to_string(), result);
    }

    /// Заставить метод отвечать ошибкой протокола.
    pub fn set_failure(&self, method: &str, error: ProtocolError) {
        self.failures.lock().unwrap().insert(method.to_string(), error);
    }

    /// Все вызовы в порядке отправки.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Только имена методов — короткая форма для проверки порядка.
    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.method).collect()
    }

    /// Вызовы конкретного метода.
    pub fn calls_of(&self, method: &str) -> Vec<RecordedCall> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == method)
            .collect()
    }

    /// Позиция первого вызова метода в общем порядке.
    pub fn index_of(&self, method: &str) -> Option<usize> {
        self.methods().iter().position(|m| m == method)
    }

    /// Испустить событие подписчикам.
    pub fn emit(&self, event: CdpEvent) {
        let _ = self.events.send(event);
    }

    /// Забыть записанное.
    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
}

#[async_trait]
impl CdpTransport for RecordingTransport {
    async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(CdpError::TransportClosed);
        }
        self.calls.lock().unwrap().push(RecordedCall {
            method: method.to_string(),
            params,
            session_id: session_id.map(str::to_owned),
        });
        if let Some(error) = self.failures.lock().unwrap().get(method).cloned() {
            return Err(CdpError::Protocol {
                method: method.to_string(),
                error,
            });
        }
        Ok(self
            .responses
            .lock()
            .unwrap()
            .get(method)
            .cloned()
            .unwrap_or_else(|| json!({})))
    }

    fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}
