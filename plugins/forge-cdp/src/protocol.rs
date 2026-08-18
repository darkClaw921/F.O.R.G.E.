//! Типы протокола Chrome DevTools Protocol и разбор входящих кадров.
//!
//! # Почему `serde_json::Value`, а не структуры на каждый метод
//!
//! CDP — большой дрейфующий протокол: несколько сотен методов, ломающиеся
//! между версиями Chrome поля, официальная генерация типов весит мегабайты.
//! Нам нужно около двадцати методов, все из его стабильного ядра
//! (`Target.*`, `Page.*`, `Runtime.*`, `Input.*`, `Browser.*`). Поэтому
//! payload'ы команд и событий держатся в `Value`: мы владеем дрейфом руками,
//! в точках, где реально читаем поля, и не платим за типы, которых не
//! используем.
//!
//! # Формат провода
//!
//! Команда: `{"id":1,"method":"Page.enable","params":{},"sessionId":"AB.."}`.
//! Ответ:   `{"id":1,"result":{...}}` либо `{"id":1,"error":{"code":-32000,"message":".."}}`.
//! Событие: `{"method":"Runtime.bindingCalled","params":{...},"sessionId":"AB.."}`.
//!
//! Единственный признак, различающий ответ и событие, — наличие поля `id`.
//! Поле `sessionId` есть у обоих и означает адресацию flat-сессии
//! (см. [`crate::transport`]).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Идентификатор flat-сессии CDP.
///
/// В flat-режиме (`Target.attachToTarget{flatten:true}`) все цели — страница,
/// её OOPIF-фреймы, воркеры — говорят по ОДНОМУ сокету, а различаются этим
/// полем. Отсутствие `session_id` означает браузерный уровень
/// (`Browser.*`, `Target.createTarget`).
pub type SessionId = String;

/// Исходящая команда протокола.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpCommand {
    /// Монотонный идентификатор, по нему роутер находит ждущий канал ответа.
    /// Уникален в пределах одного соединения, а НЕ одной сессии: в flat-режиме
    /// сокет один, и пересечение id между сессиями склеило бы чужие ответы.
    pub id: u64,
    /// Полное имя метода, `Домен.метод`.
    pub method: String,
    /// Параметры. `Value::Null` означает «параметров нет» и на провод не идёт.
    pub params: Value,
    /// Адресат. `None` — браузерный уровень.
    pub session_id: Option<SessionId>,
}

impl CdpCommand {
    /// Собирает команду. `params` принимает `serde_json::json!({...})`
    /// или `Value::Null`.
    pub fn new(
        id: u64,
        method: impl Into<String>,
        params: Value,
        session_id: Option<SessionId>,
    ) -> Self {
        Self {
            id,
            method: method.into(),
            params,
            session_id,
        }
    }

    /// Представление для провода.
    ///
    /// `params` опускается, если `Null`; `sessionId` — если `None`. Пустой
    /// объект `{}` Chrome принимает, но команды браузерного уровня без
    /// параметров читаемее без него, а логи протокола — короче.
    pub fn to_wire(&self) -> Value {
        let mut obj = Map::with_capacity(4);
        obj.insert("id".into(), Value::from(self.id));
        obj.insert("method".into(), Value::String(self.method.clone()));
        if !self.params.is_null() {
            obj.insert("params".into(), self.params.clone());
        }
        if let Some(sid) = &self.session_id {
            obj.insert("sessionId".into(), Value::String(sid.clone()));
        }
        Value::Object(obj)
    }

    /// Готовая к отправке строка (ws-кадр CDP всегда текстовый, не бинарный).
    pub fn to_json_string(&self) -> String {
        self.to_wire().to_string()
    }
}

/// Ошибка, пришедшая ОТ Chrome в ответ на команду.
///
/// Отличать её от транспортной обязательно: `-32000 Cannot find context with
/// specified id` значит «цель уехала, повторим на новой», а обрыв сокета
/// значит «браузера больше нет». Смешать их — значит либо бесконечно
/// ретраить мёртвый браузер, либо ронять встречу из-за просроченного
/// executionContextId.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// Код JSON-RPC-подобной ошибки CDP (`-32000`, `-32601`, …).
    pub code: i64,
    /// Человеческое сообщение Chrome.
    pub message: String,
    /// Необязательные подробности (`data`), у части доменов там стек JS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CDP error {}: {}", self.code, self.message)?;
        if let Some(data) = &self.data {
            write!(f, " ({data})")?;
        }
        Ok(())
    }
}

/// Ответ на команду.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpResponse {
    /// `id` исходной команды.
    pub id: u64,
    /// Сессия, из которой пришёл ответ (для диагностики; роутинг идёт по `id`).
    pub session_id: Option<SessionId>,
    /// Результат: `result` целиком либо разобранная ошибка протокола.
    pub result: Result<Value, ProtocolError>,
}

/// Событие протокола.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpEvent {
    /// Полное имя события, `Домен.событие`.
    pub method: String,
    /// Payload события; для `Runtime.bindingCalled` здесь лежит `payload`
    /// со строкой, которую страница передала в биндинг.
    pub params: Value,
    /// Сессия-источник. У браузерных событий (`Target.attachedToTarget`
    /// верхнего уровня) `None`.
    pub session_id: Option<SessionId>,
}

impl CdpEvent {
    /// Проверка соответствия фильтру подписки.
    pub fn matches(&self, filter: &EventFilter) -> bool {
        if let Some(m) = &filter.method {
            if &self.method != m {
                return false;
            }
        }
        match &filter.session_id {
            SessionFilter::Any => true,
            SessionFilter::Browser => self.session_id.is_none(),
            SessionFilter::Exact(sid) => self.session_id.as_deref() == Some(sid.as_str()),
        }
    }
}

/// По какой сессии фильтровать события.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SessionFilter {
    /// Любая, включая браузерный уровень.
    #[default]
    Any,
    /// Только браузерный уровень (`sessionId` отсутствует).
    Browser,
    /// Конкретная flat-сессия.
    Exact(SessionId),
}

/// Фильтр подписки на события: метод и/или сессия.
///
/// Нужен потому, что в flat-режиме в один broadcast-канал сыплются события
/// ВСЕХ целей: страницы, каждого OOPIF, воркеров. Подписчик, которому нужен
/// `Runtime.bindingCalled` своей встречи, обязан уметь отсечь чужое.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventFilter {
    /// Точное имя метода; `None` — любой.
    pub method: Option<String>,
    /// Ограничение по сессии.
    pub session_id: SessionFilter,
}

impl EventFilter {
    /// Фильтр «любое событие».
    pub fn any() -> Self {
        Self::default()
    }

    /// Фильтр по имени метода.
    pub fn method(method: impl Into<String>) -> Self {
        Self {
            method: Some(method.into()),
            session_id: SessionFilter::Any,
        }
    }

    /// Сузить фильтр до конкретной сессии.
    pub fn in_session(mut self, session_id: impl Into<SessionId>) -> Self {
        self.session_id = SessionFilter::Exact(session_id.into());
        self
    }

    /// Сузить фильтр до браузерного уровня.
    pub fn browser_level(mut self) -> Self {
        self.session_id = SessionFilter::Browser;
        self
    }
}

/// Разобранный входящий кадр: либо ответ, либо событие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdpMessage {
    /// Ответ на нашу команду.
    Response(CdpResponse),
    /// Событие Chrome.
    Event(CdpEvent),
}

impl CdpMessage {
    /// Разбирает текстовый кадр.
    ///
    /// Различение ответа и события — по наличию `id`, другого признака
    /// протокол не даёт. Кадр без `id` и без `method` считается битым: молча
    /// глотать такое нельзя, иначе несовместимость версии Chrome проявится
    /// как «ничего не происходит».
    pub fn parse(raw: &str) -> Result<Self, CdpError> {
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| CdpError::Malformed(format!("невалидный JSON в кадре CDP: {e}")))?;

        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);

        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let result = if let Some(err) = value.get("error") {
                Err(serde_json::from_value::<ProtocolError>(err.clone()).map_err(|e| {
                    CdpError::Malformed(format!("нераспознанный error в ответе CDP: {e}"))
                })?)
            } else {
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            };
            return Ok(CdpMessage::Response(CdpResponse {
                id,
                session_id,
                result,
            }));
        }

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            return Ok(CdpMessage::Event(CdpEvent {
                method: method.to_owned(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
                session_id,
            }));
        }

        Err(CdpError::Malformed(format!(
            "кадр CDP без id и без method: {}",
            truncate_for_log(raw, 200)
        )))
    }
}

/// Ошибки крейта.
///
/// Разделение важно для вызывающего кода `forge-meet`: на [`CdpError::Timeout`]
/// команду можно повторить, на [`CdpError::TransportClosed`] — только
/// перезапускать браузер, а [`CdpError::Protocol`] с кодом `-32000` обычно
/// означает «цель сменилась», то есть надо переподцепиться, а не падать.
#[derive(Debug)]
pub enum CdpError {
    /// Команда не получила ответа за отведённое время.
    Timeout {
        /// Метод, который не ответил.
        method: String,
        /// Отведённое время в миллисекундах.
        timeout_ms: u64,
    },
    /// Сокет закрыт: браузер умер, либо мы закрыли транспорт сами.
    /// Все ждущие команды получают именно эту ошибку — вечно висеть нельзя.
    TransportClosed,
    /// Chrome ответил ошибкой протокола.
    Protocol {
        /// Метод, на который пришла ошибка.
        method: String,
        /// Тело ошибки от Chrome.
        error: ProtocolError,
    },
    /// Кадр не разобрался: битый JSON или неизвестная форма.
    Malformed(String),
    /// Ошибка уровня WebSocket (handshake, обрыв, протокол ws).
    Ws(String),
    /// Ответ разобрался, но нужного поля в нём нет (например,
    /// `Target.attachToTarget` без `sessionId`).
    UnexpectedShape(String),
    /// Не удалось запустить Chrome или дождаться его порта отладки.
    Launch(String),
    /// Исполняемый файл Chrome не найден.
    ChromeNotFound(String),
    /// `Runtime.evaluate` вернул JS-исключение.
    JsException(String),
    /// Селектор не нашёл элемента на странице.
    ElementNotFound(String),
    /// Файловая/процессная ошибка ОС.
    Io(std::io::Error),
}

impl fmt::Display for CdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CdpError::Timeout { method, timeout_ms } => {
                write!(f, "команда CDP {method} не ответила за {timeout_ms} мс")
            }
            CdpError::TransportClosed => write!(f, "транспорт CDP закрыт"),
            CdpError::Protocol { method, error } => write!(f, "{method}: {error}"),
            CdpError::Malformed(msg) => write!(f, "битый кадр CDP: {msg}"),
            CdpError::Ws(msg) => write!(f, "ошибка WebSocket: {msg}"),
            CdpError::UnexpectedShape(msg) => write!(f, "неожиданная форма ответа CDP: {msg}"),
            CdpError::Launch(msg) => write!(f, "не удалось запустить Chrome: {msg}"),
            CdpError::ChromeNotFound(msg) => write!(f, "Chrome не найден: {msg}"),
            CdpError::JsException(msg) => write!(f, "исключение JS на странице: {msg}"),
            CdpError::ElementNotFound(sel) => write!(f, "элемент не найден: {sel}"),
            CdpError::Io(e) => write!(f, "ошибка ввода-вывода: {e}"),
        }
    }
}

impl std::error::Error for CdpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CdpError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CdpError {
    fn from(e: std::io::Error) -> Self {
        CdpError::Io(e)
    }
}

impl CdpError {
    /// Код ошибки протокола, если это она. Удобно для решения «переподцепиться
    /// или падать» без разбора текста сообщения.
    pub fn protocol_code(&self) -> Option<i64> {
        match self {
            CdpError::Protocol { error, .. } => Some(error.code),
            _ => None,
        }
    }

    /// Стоит ли повторять команду. `Timeout` — да; закрытый транспорт — нет.
    pub fn is_retryable(&self) -> bool {
        matches!(self, CdpError::Timeout { .. })
    }
}

/// Обрезает строку для лога, не разрубая UTF-8 посередине символа.
pub(crate) fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_wire_omits_empty_fields() {
        let cmd = CdpCommand::new(7, "Page.enable", Value::Null, None);
        assert_eq!(cmd.to_wire(), json!({"id": 7, "method": "Page.enable"}));

        let cmd = CdpCommand::new(
            8,
            "Page.navigate",
            json!({"url": "https://example.org"}),
            Some("S1".into()),
        );
        assert_eq!(
            cmd.to_wire(),
            json!({
                "id": 8,
                "method": "Page.navigate",
                "params": {"url": "https://example.org"},
                "sessionId": "S1"
            })
        );
    }

    #[test]
    fn parses_result_response() {
        let msg = CdpMessage::parse(r#"{"id":3,"result":{"frameId":"F1"}}"#).unwrap();
        match msg {
            CdpMessage::Response(r) => {
                assert_eq!(r.id, 3);
                assert_eq!(r.result.unwrap(), json!({"frameId": "F1"}));
            }
            other => panic!("ожидался ответ, получено {other:?}"),
        }
    }

    #[test]
    fn parses_error_response_with_code() {
        let raw = r#"{"id":4,"sessionId":"S9","error":{"code":-32000,"message":"Cannot find context"}}"#;
        match CdpMessage::parse(raw).unwrap() {
            CdpMessage::Response(r) => {
                assert_eq!(r.session_id.as_deref(), Some("S9"));
                let err = r.result.unwrap_err();
                assert_eq!(err.code, -32000);
                assert_eq!(err.message, "Cannot find context");
            }
            other => panic!("ожидался ответ, получено {other:?}"),
        }
    }

    #[test]
    fn parses_event_with_session() {
        let raw = r#"{"method":"Runtime.bindingCalled","params":{"name":"__forge_x","payload":"{}"},"sessionId":"S2"}"#;
        match CdpMessage::parse(raw).unwrap() {
            CdpMessage::Event(e) => {
                assert_eq!(e.method, "Runtime.bindingCalled");
                assert_eq!(e.session_id.as_deref(), Some("S2"));
                assert_eq!(e.params["name"], "__forge_x");
            }
            other => panic!("ожидалось событие, получено {other:?}"),
        }
    }

    #[test]
    fn frame_without_id_and_method_is_an_error_not_silence() {
        let err = CdpMessage::parse(r#"{"hello":"world"}"#).unwrap_err();
        assert!(matches!(err, CdpError::Malformed(_)), "получено {err:?}");
    }

    #[test]
    fn event_filter_separates_sessions_and_browser_level() {
        let page = CdpEvent {
            method: "Runtime.bindingCalled".into(),
            params: Value::Null,
            session_id: Some("S1".into()),
        };
        let browser = CdpEvent {
            method: "Target.attachedToTarget".into(),
            params: Value::Null,
            session_id: None,
        };

        assert!(page.matches(&EventFilter::method("Runtime.bindingCalled")));
        assert!(page.matches(&EventFilter::method("Runtime.bindingCalled").in_session("S1")));
        assert!(!page.matches(&EventFilter::method("Runtime.bindingCalled").in_session("S2")));
        assert!(!page.matches(&EventFilter::method("Runtime.bindingCalled").browser_level()));
        assert!(browser.matches(&EventFilter::any().browser_level()));
        assert!(!browser.matches(&EventFilter::method("Runtime.bindingCalled")));
    }

    #[test]
    fn errors_classify_retry_and_protocol_code() {
        let timeout = CdpError::Timeout {
            method: "Page.navigate".into(),
            timeout_ms: 30_000,
        };
        assert!(timeout.is_retryable());
        assert_eq!(timeout.protocol_code(), None);

        let proto = CdpError::Protocol {
            method: "Runtime.evaluate".into(),
            error: ProtocolError {
                code: -32000,
                message: "Cannot find context with specified id".into(),
                data: None,
            },
        };
        assert!(!proto.is_retryable());
        assert_eq!(proto.protocol_code(), Some(-32000));
        assert!(!CdpError::TransportClosed.is_retryable());
    }

    #[test]
    fn truncate_for_log_respects_utf8_boundaries() {
        let s = "привет мир";
        let cut = truncate_for_log(s, 5);
        assert!(cut.ends_with('…'));
        assert!(s.starts_with(cut.trim_end_matches('…')));
    }
}
