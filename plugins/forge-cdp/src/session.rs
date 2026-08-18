//! Flat-сессии: цели, автоаттач и гарантированное освобождение фреймов.
//!
//! # Что такое flat-режим и зачем он
//!
//! `Target.attachToTarget{flatten: true}` означает: все цели — страница, её
//! OOPIF-фреймы, воркеры — говорят по ОДНОМУ сокету, а различаются полем
//! `sessionId`. Альтернатива (вложенные сессии через `Target.sendMessageToTarget`)
//! требует упаковки команд в строку внутри команды и давно считается
//! устаревшей.
//!
//! # Почему автоаттач обязателен
//!
//! Teams и часть Google Meet живут в OOPIF: интерфейс встречи — вложенный
//! документ другого источника, в отдельном процессе рендерера. Без
//! `Target.setAutoAttach` мы к нему просто не подключимся, и инжект туда не
//! попадёт — притом что на самой странице всё будет выглядеть исправно.
//!
//! # Ловушка `waitForDebuggerOnStart`
//!
//! С `waitForDebuggerOnStart: true` новая цель СТОИТ и ждёт нас — это ровно то,
//! что нужно, чтобы успеть поставить инжект до выполнения кода фрейма. Но
//! отпустить её обязаны мы: забытый `Runtime.runIfWaitingForDebugger` вешает
//! фрейм НАМЕРТВО. Поэтому в [`AutoAttachPump`] освобождение стоит в хвосте,
//! который выполняется при ЛЮБОМ исходе настройки, включая её провал: лучше
//! фрейм без инжекта, чем замерший фрейм.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::protocol::{CdpError, CdpEvent, SessionId};
use crate::transport::SharedTransport;

/// Сессия конкретной цели.
///
/// Дёшево клонируется: внутри `Arc` на транспорт и две строки.
#[derive(Clone)]
pub struct CdpSession {
    transport: SharedTransport,
    session_id: SessionId,
    target_id: Option<String>,
}

impl std::fmt::Debug for CdpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpSession")
            .field("session_id", &self.session_id)
            .field("target_id", &self.target_id)
            .finish()
    }
}

impl CdpSession {
    /// Собирает сессию вокруг уже полученного `sessionId`.
    pub fn new(
        transport: SharedTransport,
        session_id: impl Into<SessionId>,
        target_id: Option<String>,
    ) -> Self {
        Self {
            transport,
            session_id: session_id.into(),
            target_id,
        }
    }

    /// Идентификатор сессии.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Идентификатор цели, если он известен.
    pub fn target_id(&self) -> Option<&str> {
        self.target_id.as_deref()
    }

    /// Транспорт сессии.
    pub fn transport(&self) -> &SharedTransport {
        &self.transport
    }

    /// Команда в эту сессию.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.transport
            .send(method, params, Some(&self.session_id))
            .await
    }

    /// Подписка на события (общий поток; фильтровать по
    /// [`crate::EventFilter`] с `in_session`).
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.transport.subscribe()
    }

    /// Включает домены, без которых остальное не работает.
    ///
    /// `Page` — навигация и жизненный цикл документа, `Runtime` — evaluate и
    /// СОБЫТИЕ `bindingCalled` (без `Runtime.enable` биндинг не доедет
    /// вообще), `Log` — сообщения консоли страницы: единственный способ
    /// увидеть, что наш инжект упал внутри чужого сайта.
    pub async fn enable_domains(&self) -> Result<(), CdpError> {
        self.call("Page.enable", json!({})).await?;
        self.call("Runtime.enable", json!({})).await?;
        // Log.enable — диагностика, а не механика: не даём ей уронить старт
        // встречи, если домен недоступен на этой цели (воркеры его не имеют).
        if let Err(e) = self.call("Log.enable", json!({})).await {
            tracing::debug!(session = %self.session_id, error = %e, "forge-cdp: Log.enable недоступен");
        }
        Ok(())
    }

    /// Включает автоаттач к дочерним целям этой сессии.
    pub async fn set_auto_attach(&self, wait_for_debugger: bool) -> Result<(), CdpError> {
        self.call(
            "Target.setAutoAttach",
            json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": wait_for_debugger,
                "flatten": true,
            }),
        )
        .await
        .map(|_| ())
    }

    /// Отпускает цель, замершую по `waitForDebuggerOnStart`.
    pub async fn run_if_waiting_for_debugger(&self) -> Result<(), CdpError> {
        self.call("Runtime.runIfWaitingForDebugger", json!({}))
            .await
            .map(|_| ())
    }
}

/// Настройка новой цели, найденной автоаттачем.
///
/// Реализуется в [`crate::inject`]: именно сюда попадает установка биндинга и
/// скрипта нового документа для каждого OOPIF-фрейма.
#[async_trait]
pub trait SessionSetup: Send + Sync + 'static {
    /// Настроить дочернюю сессию. Ошибка логируется, но НЕ отменяет
    /// освобождение цели.
    async fn setup(&self, session: &CdpSession) -> Result<(), CdpError>;
}

/// Параметры насоса автоаттача.
#[derive(Debug, Clone)]
pub struct AutoAttachConfig {
    /// Ставить ли `waitForDebuggerOnStart` дочерним целям. Практически всегда
    /// `true`: иначе фрейм успевает выполнить свой код до инжекта.
    pub wait_for_debugger: bool,
    /// Транзитивно включать автоаттач на самих дочерних сессиях — иначе
    /// вложенный в OOPIF ещё один OOPIF окажется вне поля зрения.
    pub auto_attach_children: bool,
    /// Сессии, которые насос обязан пропустить (обычно главная: её настроил
    /// вызывающий код, повторный инжект дал бы ДВЕ копии скрипта в документе).
    pub ignore_sessions: Vec<SessionId>,
}

impl Default for AutoAttachConfig {
    fn default() -> Self {
        Self {
            wait_for_debugger: true,
            auto_attach_children: true,
            ignore_sessions: Vec::new(),
        }
    }
}

/// Фоновая задача, обрабатывающая `Target.attachedToTarget`.
pub struct AutoAttachPump {
    handle: JoinHandle<()>,
    ignored: Arc<Mutex<HashSet<SessionId>>>,
}

impl AutoAttachPump {
    /// Добавить сессию в список игнорируемых.
    pub fn ignore(&self, session_id: impl Into<SessionId>) {
        self.ignored.lock().unwrap().insert(session_id.into());
    }

    /// Остановить насос.
    pub fn stop(&self) {
        self.handle.abort();
    }
}

impl Drop for AutoAttachPump {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Браузерный уровень: цели, разрешения, автоаттач.
#[derive(Clone)]
pub struct CdpBrowser {
    transport: SharedTransport,
}

impl CdpBrowser {
    /// Оборачивает транспорт.
    pub fn new(transport: SharedTransport) -> Self {
        Self { transport }
    }

    /// Транспорт браузера.
    pub fn transport(&self) -> SharedTransport {
        self.transport.clone()
    }

    /// Команда браузерного уровня (без `sessionId`).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.transport.send(method, params, None).await
    }

    /// Создаёт вкладку и возвращает `targetId`.
    ///
    /// `background: true` НАМЕРЕННО: цель бота не должна выходить на передний
    /// план. Вместе с `--no-startup-window` это и есть выполнение требования
    /// «браузер не отбирает фокус у пользователя»: окна при старте нет, а
    /// созданная по протоколу вкладка не активируется.
    pub async fn create_target(&self, url: &str) -> Result<String, CdpError> {
        self.create_target_with(url, true).await
    }

    /// То же с явным выбором, выводить ли цель на передний план.
    ///
    /// `background: false` осмыслен ровно в одном случае — человек сам
    /// попросил показать окно бота (ассистированный вход).
    pub async fn create_target_with(
        &self,
        url: &str,
        background: bool,
    ) -> Result<String, CdpError> {
        let res = self
            .call(
                "Target.createTarget",
                json!({ "url": url, "background": background }),
            )
            .await?;
        res["targetId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| CdpError::UnexpectedShape(format!("Target.createTarget без targetId: {res}")))
    }

    /// Подключается к цели в плоском режиме.
    pub async fn attach(&self, target_id: &str) -> Result<CdpSession, CdpError> {
        let res = self
            .call(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session_id = res["sessionId"].as_str().map(str::to_owned).ok_or_else(|| {
            CdpError::UnexpectedShape(format!("Target.attachToTarget без sessionId: {res}"))
        })?;
        Ok(CdpSession::new(
            self.transport.clone(),
            session_id,
            Some(target_id.to_string()),
        ))
    }

    /// Создаёт вкладку, подключается к ней и включает домены.
    ///
    /// URL здесь — всегда `about:blank`: настоящая навигация идёт после
    /// инжекта, иначе патчи опоздают (см. [`crate::page::CdpPage::prepare_and_navigate`]).
    pub async fn open_target(&self, url: &str) -> Result<CdpSession, CdpError> {
        let target_id = self.create_target(url).await?;
        let session = self.attach(&target_id).await?;
        session.enable_domains().await?;
        Ok(session)
    }

    /// Закрывает цель.
    pub async fn close_target(&self, target_id: &str) -> Result<(), CdpError> {
        self.call("Target.closeTarget", json!({ "targetId": target_id }))
            .await
            .map(|_| ())
    }

    /// Выдаёт разрешения источнику встречи.
    ///
    /// `--use-fake-ui-for-media-stream` уже снимает диалог, но это ВТОРОЙ,
    /// независимый барьер: флаг командной строки можно потерять при правке
    /// конфига, а тихо не выданный микрофон означает встречу без звука.
    pub async fn grant_permissions(
        &self,
        origin: Option<&str>,
        permissions: &[&str],
    ) -> Result<(), CdpError> {
        let mut params = json!({ "permissions": permissions });
        if let Some(origin) = origin {
            params["origin"] = Value::String(origin.to_string());
        }
        self.call("Browser.grantPermissions", params).await.map(|_| ())
    }

    /// Запускает насос автоаттача.
    ///
    /// Порядок важен: насос должен подписаться на события ДО того, как
    /// `Target.setAutoAttach` будет вызван на главной сессии. Иначе первые
    /// же фреймы прилетят в пустоту и останутся висеть навсегда.
    pub fn spawn_auto_attach(
        &self,
        setup: Option<Arc<dyn SessionSetup>>,
        cfg: AutoAttachConfig,
    ) -> AutoAttachPump {
        let ignored = Arc::new(Mutex::new(
            cfg.ignore_sessions.iter().cloned().collect::<HashSet<_>>(),
        ));
        let transport = self.transport.clone();
        let ignored_task = ignored.clone();
        let handle = tokio::spawn(async move {
            let mut rx = transport.subscribe();
            let mut seen: HashSet<SessionId> = HashSet::new();
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.method != "Target.attachedToTarget" {
                            continue;
                        }
                        let Some(sid) = event.params["sessionId"].as_str() else {
                            continue;
                        };
                        if ignored_task.lock().unwrap().contains(sid) {
                            continue;
                        }
                        if !seen.insert(sid.to_string()) {
                            continue;
                        }
                        let waiting = event.params["waitingForDebugger"]
                            .as_bool()
                            .unwrap_or(false);
                        let target_id = event.params["targetInfo"]["targetId"]
                            .as_str()
                            .map(str::to_owned);
                        let target_type = event.params["targetInfo"]["type"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string();
                        let child =
                            CdpSession::new(transport.clone(), sid.to_string(), target_id);
                        let setup = setup.clone();
                        let auto_children = cfg.auto_attach_children;
                        let wait_children = cfg.wait_for_debugger;
                        tokio::spawn(async move {
                            handle_attached_target(
                                child,
                                setup,
                                waiting,
                                auto_children,
                                wait_children,
                                &target_type,
                            )
                            .await;
                        });
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Отставание насоса означает, что мы могли пропустить
                        // прикрепление фрейма, и он останется замершим. Это
                        // диагностируемая беда, а не мелочь.
                        tracing::warn!(
                            lost = n,
                            "forge-cdp: насос автоаттача отстал, возможен пропущенный фрейм"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        AutoAttachPump { handle, ignored }
    }
}

/// Настраивает дочернюю цель и ОБЯЗАТЕЛЬНО её отпускает.
///
/// Хвост с `runIfWaitingForDebugger` выполняется при любом исходе настройки:
/// фрейм без инжекта — потеря данных, замерший фрейм — сломанная встреча.
async fn handle_attached_target(
    session: CdpSession,
    setup: Option<Arc<dyn SessionSetup>>,
    waiting: bool,
    auto_attach_children: bool,
    wait_children: bool,
    target_type: &str,
) {
    tracing::debug!(
        session = %session.session_id(),
        target_type,
        waiting,
        "forge-cdp: автоаттач к новой цели"
    );

    if let Err(e) = session.enable_domains().await {
        tracing::warn!(session = %session.session_id(), error = %e, "forge-cdp: домены дочерней цели не включились");
    }

    if let Some(setup) = setup {
        if let Err(e) = setup.setup(&session).await {
            tracing::warn!(
                session = %session.session_id(),
                error = %e,
                "forge-cdp: настройка дочерней цели провалилась, цель всё равно будет отпущена"
            );
        }
    }

    if auto_attach_children {
        if let Err(e) = session.set_auto_attach(wait_children).await {
            tracing::debug!(session = %session.session_id(), error = %e, "forge-cdp: setAutoAttach на дочерней цели не удался");
        }
    }

    if waiting {
        if let Err(e) = session.run_if_waiting_for_debugger().await {
            tracing::warn!(
                session = %session.session_id(),
                error = %e,
                "forge-cdp: цель НЕ отпущена — фрейм останется замершим"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ProtocolError;
    use crate::testing::RecordingTransport;
    use std::time::Duration;

    /// Ждёт выполнения условия, опрашивая записанные вызовы.
    async fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("не дождались: {what}");
    }

    fn attached_event(session_id: &str, waiting: bool) -> CdpEvent {
        CdpEvent {
            method: "Target.attachedToTarget".into(),
            params: json!({
                "sessionId": session_id,
                "waitingForDebugger": waiting,
                "targetInfo": {"targetId": "T-CHILD", "type": "iframe", "url": "https://cdn.example/frame"}
            }),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn open_target_creates_attaches_flat_and_enables_domains() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let session = browser.open_target("about:blank").await.unwrap();
        assert_eq!(session.session_id(), "SESSION-FAKE");
        assert_eq!(
            t.methods(),
            vec![
                "Target.createTarget",
                "Target.attachToTarget",
                "Page.enable",
                "Runtime.enable",
                "Log.enable",
            ]
        );
        // Вкладка бота создаётся в фоне: выход на передний план отобрал бы
        // фокус у пользователя.
        assert_eq!(
            t.calls_of("Target.createTarget")[0].params["background"],
            true
        );
        let attach = &t.calls_of("Target.attachToTarget")[0];
        assert_eq!(
            attach.params["flatten"], true,
            "без flatten вложенные сессии требуют упаковки команд в строку"
        );
        // Домены включаются в СВОЕЙ сессии, а не браузерным уровнем.
        assert_eq!(
            t.calls_of("Page.enable")[0].session_id.as_deref(),
            Some("SESSION-FAKE")
        );
    }

    /// Цель, пришедшая с `waitingForDebugger`, ГАРАНТИРОВАННО отпускается.
    #[tokio::test]
    async fn attached_target_is_always_released() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let _pump = browser.spawn_auto_attach(None, AutoAttachConfig::default());
        // Насос уже подписан — только после этого шлём событие.
        tokio::time::sleep(Duration::from_millis(20)).await;
        t.emit(attached_event("CHILD-1", true));

        wait_until(
            || !t.calls_of("Runtime.runIfWaitingForDebugger").is_empty(),
            "Runtime.runIfWaitingForDebugger для дочерней цели",
        )
        .await;
        let call = &t.calls_of("Runtime.runIfWaitingForDebugger")[0];
        assert_eq!(call.session_id.as_deref(), Some("CHILD-1"));

        // Автоаттач транзитивен: вложенный OOPIF тоже будет пойман.
        let auto = t.calls_of("Target.setAutoAttach");
        assert!(auto.iter().any(|c| c.session_id.as_deref() == Some("CHILD-1")));
        // И порядок: setAutoAttach ДО освобождения, иначе внуки убегут.
        let idx_auto = t
            .methods()
            .iter()
            .position(|m| m == "Target.setAutoAttach")
            .unwrap();
        let idx_run = t.index_of("Runtime.runIfWaitingForDebugger").unwrap();
        assert!(idx_auto < idx_run);
    }

    /// Провал настройки НЕ имеет права оставить фрейм замершим.
    #[tokio::test]
    async fn target_is_released_even_when_setup_fails() {
        struct FailingSetup;
        #[async_trait]
        impl SessionSetup for FailingSetup {
            async fn setup(&self, _s: &CdpSession) -> Result<(), CdpError> {
                Err(CdpError::JsException("инжект упал".into()))
            }
        }

        let t = RecordingTransport::new();
        // Домены тоже отвечают ошибкой — худший случай.
        t.set_failure(
            "Page.enable",
            ProtocolError {
                code: -32000,
                message: "no page".into(),
                data: None,
            },
        );
        let browser = CdpBrowser::new(t.clone());
        let _pump =
            browser.spawn_auto_attach(Some(Arc::new(FailingSetup)), AutoAttachConfig::default());
        tokio::time::sleep(Duration::from_millis(20)).await;
        t.emit(attached_event("CHILD-2", true));

        wait_until(
            || !t.calls_of("Runtime.runIfWaitingForDebugger").is_empty(),
            "освобождение цели после провала настройки",
        )
        .await;
        assert_eq!(
            t.calls_of("Runtime.runIfWaitingForDebugger")[0]
                .session_id
                .as_deref(),
            Some("CHILD-2")
        );
    }

    /// Цель без `waitingForDebugger` отпускать не нужно и не следует.
    #[tokio::test]
    async fn target_not_waiting_is_not_released() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let _pump = browser.spawn_auto_attach(None, AutoAttachConfig::default());
        tokio::time::sleep(Duration::from_millis(20)).await;
        t.emit(attached_event("CHILD-3", false));

        wait_until(
            || !t.calls_of("Target.setAutoAttach").is_empty(),
            "обработку цели без ожидания отладчика",
        )
        .await;
        assert!(t.calls_of("Runtime.runIfWaitingForDebugger").is_empty());
    }

    /// Главная сессия в список игнора — иначе в документе окажутся ДВЕ копии
    /// нашего скрипта.
    #[tokio::test]
    async fn ignored_session_is_not_touched_by_the_pump() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let cfg = AutoAttachConfig {
            ignore_sessions: vec!["MAIN".into()],
            ..AutoAttachConfig::default()
        };
        let _pump = browser.spawn_auto_attach(None, cfg);
        tokio::time::sleep(Duration::from_millis(20)).await;
        t.emit(attached_event("MAIN", true));
        t.emit(attached_event("CHILD-4", true));

        wait_until(
            || {
                t.calls_of("Runtime.runIfWaitingForDebugger")
                    .iter()
                    .any(|c| c.session_id.as_deref() == Some("CHILD-4"))
            },
            "обработку неигнорируемой цели",
        )
        .await;
        assert!(
            t.calls()
                .iter()
                .all(|c| c.session_id.as_deref() != Some("MAIN")),
            "в игнорируемую сессию не должно уйти ни одной команды насоса"
        );
    }

    #[tokio::test]
    async fn duplicate_attach_events_are_processed_once() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let _pump = browser.spawn_auto_attach(None, AutoAttachConfig::default());
        tokio::time::sleep(Duration::from_millis(20)).await;
        t.emit(attached_event("CHILD-5", true));
        t.emit(attached_event("CHILD-5", true));

        wait_until(
            || !t.calls_of("Runtime.runIfWaitingForDebugger").is_empty(),
            "освобождение цели",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            t.calls_of("Runtime.runIfWaitingForDebugger").len(),
            1,
            "повторное событие о той же сессии не должно давать второй инжект"
        );
    }

    #[tokio::test]
    async fn grant_permissions_reaches_browser_level() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        browser
            .grant_permissions(
                Some("https://telemost.yandex.ru"),
                &["audioCapture", "videoCapture"],
            )
            .await
            .unwrap();
        let call = &t.calls_of("Browser.grantPermissions")[0];
        assert_eq!(call.session_id, None, "разрешения выдаются браузером, не сессией");
        assert_eq!(call.params["origin"], "https://telemost.yandex.ru");
        assert_eq!(call.params["permissions"][0], "audioCapture");
    }
}
