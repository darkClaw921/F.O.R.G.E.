//! Доставка нашего скрипта в страницу и канал данных из страницы в Rust.
//!
//! # Почему инжект вообще возможен только так
//!
//! Живая проба Яндекс Телемоста (фаза 0) прямым тестом показала:
//! `cspBlocksInlineScript: true` — политика безопасности страницы НЕ ДАЁТ
//! выполнить инлайн-скрипт. Из этого следуют два вывода, оба несущие.
//!
//! 1. Подход «расширение Chrome / контент-скрипт» нежизнеспособен: контент-скрипт
//!    живёт в изолированном мире и не видит `window.RTCPeerConnection`
//!    страницы, а инъекция в главный мир режется CSP.
//! 2. `Page.setBypassCSP(true)` — не страховка, а условие работы, и звать его
//!    надо ДО навигации.
//!
//! # Почему строго до навигации
//!
//! `Runtime.evaluate` после загрузки уже поздно: `RTCPeerConnection` сайта
//! создан, соединение установлено, патчить нечего. Нужен
//! `Page.addScriptToEvaluateOnNewDocument`, который выполняется раньше любого
//! кода документа.
//!
//! # Канал данных
//!
//! `Runtime.addBinding` создаёт в `window` функцию; вызов её из страницы
//! приходит в Rust событием `Runtime.bindingCalled`. Имя — СЛУЧАЙНОЕ НА
//! ВСТРЕЧУ (`__forge_<32 hex>`): постоянное имя это, во-первых, готовый
//! признак автоматизации для антибот-логики сайта, во-вторых, риск
//! столкновения двух ботов в одном профиле.
//!
//! # Две неочевидности, из-за которых теряются данные
//!
//! **Гигиена.** Наш скрипт выполняется раньше кода сайта и первым делом
//! забирает функцию в замыкание и удаляет глобал: код страницы не должен
//! видеть постороннюю функцию в `window`.
//!
//! **Поллинг обязателен.** Порядок «биндинг привязан» и «скрипт нового
//! документа выполнен» протоколом НЕ ГАРАНТИРОВАН. Если скрипт стартовал
//! раньше биндинга, функции в `window` ещё нет — и кадры до её появления
//! копятся в кольцевом буфере фиксированного размера, а при переполнении
//! считается счётчик потерь, который уезжает в Rust отдельным служебным
//! сообщением. Молчаливая потеря первых секунд записи — худший из возможных
//! исходов: она незаметна.

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::broadcast;

use crate::protocol::{CdpError, CdpEvent, SessionId};
use crate::session::{CdpSession, SessionSetup};

/// Тип служебного сообщения из страницы (поле `__forge`).
pub const BINDING_META_TYPE: &str = "meta";

/// Генерирует имя биндинга, уникальное для встречи.
///
/// 128 бит из UUID v4: угадать имя, чтобы перехватить канал или обнаружить
/// бота по фиксированной строке, нельзя.
pub fn random_binding_name() -> String {
    format!("__forge_{}", uuid::Uuid::new_v4().simple())
}

/// Параметры инжекта.
#[derive(Debug, Clone)]
pub struct InjectConfig {
    /// Имя функции-биндинга в `window`. По умолчанию случайное.
    pub binding_name: String,
    /// Ёмкость кольцевого буфера кадров, накопленных до появления функции.
    ///
    /// Буфер именно кольцевой: при переполнении выбрасывается САМОЕ СТАРОЕ.
    /// Для аудио свежие кадры ценнее просроченных, а неограниченный буфер на
    /// странице, где биндинг так и не появился, съел бы память вкладки.
    pub ring_capacity: usize,
    /// Шаг опроса `window[binding]`, мс.
    pub poll_interval_ms: u64,
    /// Наш скрипт. Выполняется ВНУТРИ той же функции, где живёт `post`,
    /// поэтому не создаёт ни одного глобала.
    pub user_script: String,
    /// Выполнить скрипт и в уже существующих контекстах (`runImmediately`).
    ///
    /// По умолчанию `false` — протокольное значение. Включать опасно:
    /// в контексте, который получит скрипт и как «новый документ», и как
    /// «существующий», он выполнится ДВАЖДЫ, а два обёрнутых
    /// `RTCPeerConnection` дадут двойной захват. Поле оставлено ради фаз, где
    /// придётся цепляться к уже открытой странице.
    pub run_immediately: bool,
}

impl Default for InjectConfig {
    fn default() -> Self {
        Self {
            binding_name: random_binding_name(),
            // 4096 кадров PCM по 20 мс это ~80 секунд речи — с запасом
            // перекрывает окно между стартом документа и привязкой биндинга,
            // которое измеряется миллисекундами.
            ring_capacity: 4096,
            poll_interval_ms: 10,
            user_script: String::new(),
            run_immediately: false,
        }
    }
}

impl InjectConfig {
    /// Конфигурация с заданным скриптом и случайным именем биндинга.
    pub fn with_script(user_script: impl Into<String>) -> Self {
        Self {
            user_script: user_script.into(),
            ..Self::default()
        }
    }
}

/// Собирает исходник, который поедет в `Page.addScriptToEvaluateOnNewDocument`.
///
/// Устройство результата:
/// * всё внутри одной анонимной функции — ни одного глобала;
/// * первым делом `grab()` забирает функцию биндинга и УДАЛЯЕТ её из `window`;
/// * пока функции нет, `post()` копит кадры в кольцевом буфере и считает
///   потери;
/// * появление функции проверяется по таймеру (порядок с
///   `Runtime.addBinding` протоколом не задан);
/// * пользовательский скрипт вставляется последним и видит `post` в области
///   видимости.
pub fn build_inject_source(cfg: &InjectConfig) -> String {
    // Имя и числа кладём через JSON — исходник собирается из данных, и
    // экранирование не должно зависеть от того, что в них лежит.
    let name = serde_json::to_string(&cfg.binding_name).unwrap_or_else(|_| "\"__forge\"".into());
    let cap = cfg.ring_capacity.max(1);
    let poll = cfg.poll_interval_ms.max(1);
    let meta = serde_json::to_string(BINDING_META_TYPE).unwrap_or_else(|_| "\"meta\"".into());
    let user = &cfg.user_script;

    format!(
        r#"(function () {{
  "use strict";
  var NAME = {name};
  var CAP = {cap};
  var POLL = {poll};
  var META = {meta};
  var send = null;
  var buf = [];
  var dropped = 0;
  var reported = -1;

  // Забирает функцию биндинга в замыкание и убирает её из window: код сайта
  // не должен видеть постороннюю функцию.
  function grab() {{
    var f;
    try {{ f = window[NAME]; }} catch (e) {{ return false; }}
    if (typeof f !== "function") return false;
    send = f;
    try {{ delete window[NAME]; }} catch (e) {{
      try {{ window[NAME] = undefined; }} catch (e2) {{}}
    }}
    return true;
  }}

  function raw(s) {{
    try {{ send(s); return true; }} catch (e) {{ send = null; return false; }}
  }}

  function flush() {{
    while (send && buf.length) {{
      var next = buf.shift();
      if (!raw(next)) {{ buf.unshift(next); return; }}
    }}
  }}

  // Служебное сообщение со счётчиком потерь. Уходит только когда счётчик
  // изменился — иначе оно забило бы канал при постоянном переполнении.
  function meta() {{
    if (send && dropped !== reported) {{
      reported = dropped;
      raw(JSON.stringify({{ __forge: META, dropped: dropped, buffered: buf.length }}));
    }}
  }}

  function post(msg) {{
    var s = typeof msg === "string" ? msg : JSON.stringify(msg);
    if (send) {{
      flush();
      if (raw(s)) {{ meta(); return true; }}
    }}
    if (buf.length >= CAP) {{ buf.shift(); dropped++; }}
    buf.push(s);
    return false;
  }}

  if (!grab()) {{
    var timer = setInterval(function () {{
      if (grab()) {{ clearInterval(timer); flush(); meta(); }}
    }}, POLL);
  }} else {{
    meta();
  }}

  // --- пользовательский скрипт: видит post, не видит window[NAME] ---
{user}
}})();
"#
    )
}

/// Разобранное сообщение из страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingMessage {
    /// Служебное: сколько кадров потеряно и сколько сейчас в буфере.
    Meta {
        /// Накопленный счётчик потерь.
        dropped: u64,
        /// Сколько кадров лежит в буфере на момент отправки.
        buffered: u64,
    },
    /// Полезная нагрузка нашего скрипта, как есть.
    Payload(String),
}

/// Разбирает `payload` из `Runtime.bindingCalled`.
pub fn parse_binding_payload(payload: &str) -> BindingMessage {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        if v.get("__forge").and_then(|x| x.as_str()) == Some(BINDING_META_TYPE) {
            return BindingMessage::Meta {
                dropped: v.get("dropped").and_then(|x| x.as_u64()).unwrap_or(0),
                buffered: v.get("buffered").and_then(|x| x.as_u64()).unwrap_or(0),
            };
        }
    }
    BindingMessage::Payload(payload.to_string())
}

/// Приёмник сообщений биндинга.
///
/// Отфильтровывает чужие биндинги по имени (в одном браузере может идти
/// несколько встреч) и сам логирует потери кадров: счётчик из служебного
/// сообщения обязан быть виден, иначе тихая потеря первых секунд записи
/// останется незамеченной.
pub struct BindingReceiver {
    rx: broadcast::Receiver<CdpEvent>,
    name: String,
}

impl BindingReceiver {
    /// Создаёт приёмник поверх подписки на события.
    pub fn new(rx: broadcast::Receiver<CdpEvent>, name: impl Into<String>) -> Self {
        Self {
            rx,
            name: name.into(),
        }
    }

    /// Имя биндинга, который слушает приёмник.
    pub fn binding_name(&self) -> &str {
        &self.name
    }

    /// Ждёт следующее сообщение. Возвращает сессию-источник и сообщение.
    ///
    /// Сессия важна: кадры из OOPIF-фрейма приходят со своим `sessionId`, и
    /// по нему видно, какой именно документ говорит.
    pub async fn recv(&mut self) -> Result<(Option<SessionId>, BindingMessage), CdpError> {
        loop {
            match self.rx.recv().await {
                Ok(event) => {
                    if event.method != "Runtime.bindingCalled" {
                        continue;
                    }
                    if event.params["name"].as_str() != Some(self.name.as_str()) {
                        continue;
                    }
                    let payload = event.params["payload"].as_str().unwrap_or_default();
                    let msg = parse_binding_payload(payload);
                    if let BindingMessage::Meta { dropped, buffered } = &msg {
                        if *dropped > 0 {
                            tracing::warn!(
                                dropped,
                                buffered,
                                session = ?event.session_id,
                                "forge-cdp: страница потеряла кадры до привязки биндинга"
                            );
                        }
                    }
                    return Ok((event.session_id, msg));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        lost = n,
                        "forge-cdp: приёмник биндинга отстал, кадры потеряны в Rust"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => return Err(CdpError::TransportClosed),
            }
        }
    }
}

/// Установка инжекта в сессию.
///
/// Реализует [`SessionSetup`], поэтому одна и та же настройка применяется и к
/// главной странице, и к каждому OOPIF-фрейму, пойманному автоаттачем.
#[derive(Debug, Clone)]
pub struct InjectSetup {
    /// Параметры инжекта; имя биндинга общее для всех фреймов встречи.
    pub config: InjectConfig,
    /// Звать ли `Page.setBypassCSP`. Практически всегда `true`: без него
    /// инжект в страницу с политикой безопасности не выполнится (измерено на
    /// Телемосте).
    pub bypass_csp: bool,
}

impl InjectSetup {
    /// Настройка с указанным скриптом.
    pub fn new(config: InjectConfig) -> Self {
        Self {
            config,
            bypass_csp: true,
        }
    }

    /// Применяет `setBypassCSP` + `addBinding` + `addScriptToEvaluateOnNewDocument`
    /// в указанном порядке.
    ///
    /// Порядок нарушать нельзя: CSP снимается ДО того, как что-либо
    /// выполняется, а биндинг привязывается ДО скрипта, чтобы окно ожидания в
    /// кольцевом буфере было как можно короче.
    pub async fn apply(&self, session: &CdpSession) -> Result<(), CdpError> {
        if self.bypass_csp {
            session
                .call("Page.setBypassCSP", json!({"enabled": true}))
                .await?;
        }
        session
            .call(
                "Runtime.addBinding",
                json!({"name": self.config.binding_name}),
            )
            .await?;
        let source = build_inject_source(&self.config);
        let mut params = json!({"source": source});
        if self.config.run_immediately {
            params["runImmediately"] = json!(true);
        }
        session
            .call("Page.addScriptToEvaluateOnNewDocument", params)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl SessionSetup for InjectSetup {
    async fn setup(&self, session: &CdpSession) -> Result<(), CdpError> {
        self.apply(session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::CdpBrowser;
    use crate::testing::RecordingTransport;
    use crate::transport::CdpTransport;

    #[test]
    fn binding_name_is_random_per_meeting() {
        let a = random_binding_name();
        let b = random_binding_name();
        assert_ne!(a, b, "постоянное имя биндинга — признак автоматизации");
        assert!(a.starts_with("__forge_"));
        assert_eq!(a.len(), "__forge_".len() + 32);
        assert!(a["__forge_".len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn source_hides_the_global_and_creates_no_new_ones() {
        let cfg = InjectConfig {
            binding_name: "__forge_test".into(),
            user_script: "post('hi');".into(),
            ..InjectConfig::default()
        };
        let src = build_inject_source(&cfg);

        assert!(src.starts_with("(function ()"), "всё обязано быть в IIFE");
        assert!(src.contains(r#"var NAME = "__forge_test";"#));
        assert!(
            src.contains("delete window[NAME]"),
            "глобал должен исчезать из window сразу после захвата"
        );
        assert!(src.contains("post('hi');"), "скрипт пользователя должен быть внутри");

        // Единственные записи в window — стирание нашего же имени.
        for line in src.lines() {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            if let Some(pos) = line.find("window[") {
                let tail = &line[pos..];
                assert!(
                    tail.starts_with("window[NAME]"),
                    "инжект не имеет права трогать другие свойства window: {line}"
                );
            }
            assert!(
                !line.contains("window.__") && !line.contains("globalThis"),
                "инжект не имеет права заводить глобалы: {line}"
            );
        }
    }

    #[test]
    fn source_carries_ring_buffer_and_polling() {
        let cfg = InjectConfig {
            ring_capacity: 128,
            poll_interval_ms: 7,
            ..InjectConfig::default()
        };
        let src = build_inject_source(&cfg);
        assert!(src.contains("var CAP = 128;"));
        assert!(src.contains("var POLL = 7;"));
        // Поллинг обязателен: порядок addBinding и скрипта не гарантирован.
        assert!(src.contains("setInterval("));
        // Кольцо: при переполнении выбрасывается самое старое, счётчик растёт.
        assert!(src.contains("buf.shift(); dropped++;"));
        assert!(src.contains("dropped: dropped"));
    }

    #[test]
    fn zero_capacity_and_zero_poll_are_clamped() {
        let cfg = InjectConfig {
            ring_capacity: 0,
            poll_interval_ms: 0,
            ..InjectConfig::default()
        };
        let src = build_inject_source(&cfg);
        // setInterval(0) сжигал бы процессор страницы встречи.
        assert!(src.contains("var POLL = 1;"));
        assert!(src.contains("var CAP = 1;"));
    }

    #[test]
    fn meta_payload_is_recognised_and_user_payload_is_not_touched() {
        match parse_binding_payload(r#"{"__forge":"meta","dropped":12,"buffered":4}"#) {
            BindingMessage::Meta { dropped, buffered } => {
                assert_eq!(dropped, 12);
                assert_eq!(buffered, 4);
            }
            other => panic!("ожидалось служебное сообщение, получено {other:?}"),
        }
        // Сообщение приложения с похожим полем не должно приниматься за
        // служебное.
        assert_eq!(
            parse_binding_payload(r#"{"__forge":"frame","t":1}"#),
            BindingMessage::Payload(r#"{"__forge":"frame","t":1}"#.to_string())
        );
        assert_eq!(
            parse_binding_payload("не json вовсе"),
            BindingMessage::Payload("не json вовсе".to_string())
        );
    }

    #[tokio::test]
    async fn inject_setup_sends_bypass_binding_and_script_in_order() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();

        let cfg = InjectConfig {
            binding_name: "__forge_abc".into(),
            user_script: "post('x');".into(),
            ..InjectConfig::default()
        };
        InjectSetup::new(cfg).apply(&session).await.unwrap();

        assert_eq!(
            t.methods(),
            vec![
                "Page.setBypassCSP",
                "Runtime.addBinding",
                "Page.addScriptToEvaluateOnNewDocument",
            ]
        );
        assert_eq!(t.calls_of("Page.setBypassCSP")[0].params["enabled"], true);
        assert_eq!(t.calls_of("Runtime.addBinding")[0].params["name"], "__forge_abc");
        let src = t.calls_of("Page.addScriptToEvaluateOnNewDocument")[0].params["source"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(src.contains("__forge_abc"));
        assert!(src.contains("post('x');"));
        // runImmediately по умолчанию не ставится: двойное выполнение в одном
        // контексте дало бы двойной захват.
        assert!(t.calls_of("Page.addScriptToEvaluateOnNewDocument")[0].params
            .get("runImmediately")
            .is_none());
    }

    #[tokio::test]
    async fn binding_receiver_filters_by_name_and_reports_losses() {
        let t = RecordingTransport::new();
        let mut rx = BindingReceiver::new(t.subscribe(), "__forge_mine");

        t.emit(CdpEvent {
            method: "Runtime.bindingCalled".into(),
            params: json!({"name": "__forge_other", "payload": "чужое"}),
            session_id: Some("S1".into()),
        });
        t.emit(CdpEvent {
            method: "Runtime.bindingCalled".into(),
            params: json!({"name": "__forge_mine", "payload": r#"{"__forge":"meta","dropped":3,"buffered":0}"#}),
            session_id: Some("S2".into()),
        });
        t.emit(CdpEvent {
            method: "Runtime.bindingCalled".into(),
            params: json!({"name": "__forge_mine", "payload": "кадр"}),
            session_id: Some("S2".into()),
        });

        let (session, msg) = rx.recv().await.unwrap();
        assert_eq!(session.as_deref(), Some("S2"));
        assert_eq!(
            msg,
            BindingMessage::Meta {
                dropped: 3,
                buffered: 0
            }
        );
        let (_, msg) = rx.recv().await.unwrap();
        assert_eq!(msg, BindingMessage::Payload("кадр".into()));
    }
}
