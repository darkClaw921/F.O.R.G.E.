//! Action-кнопки под assistant-сообщениями (Phase 5b).
//!
//! ## Зачем
//!
//! После того как Claude сгенерировал ответ, он может предложить пользователю
//! интерактивные кнопки — «Run command», «Create task», «Open session» и т.п.
//! Это делается через специальный markdown-блок в финальном тексте — fenced
//! code block с языком `forge-actions`, тело — JSON-массив описаний actions.
//! Каждый объект содержит поля: `id`, `label`, `kind` (`prompt` или `system`),
//! и либо `text` (для prompt), либо `name`+`params` (для system).
//!
//! Парсер ([`parser::extract`]) вытаскивает такие блоки из ответа и эмитит
//! список [`Action`]. WS-handler рассылает их как
//! [`crate::ws::protocol::ServerMsg::ActionButtons`].
//!
//! ## Безопасность
//!
//! `System`-actions выполняются только под пользовательским подтверждением
//! (см. [`executor::invoke`]) и **запрещены** в autonomous-контексте — это
//! защищает от того, что фоновая задача через подсказку Claude переключит
//! tmux-сессию или создаст task без ведома пользователя.

pub mod executor;
pub mod parser;

use serde::{Deserialize, Serialize};

/// Whitelist системных действий. Любое имя вне этого enum'а парсер
/// отбрасывает с warning'ом.
///
/// Сериализация в snake_case → JSON-имя совпадает с тем, что Claude должен
/// писать в `name`. Добавление нового варианта — намеренное расширение
/// поверхности атаки, требует review (Phase 6 hardening).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemActionKind {
    /// Открыть существующую tmux-сессию по имени. `params.name: String`.
    OpenSession,
    /// Перезапустить tmux-сессию. `params.name: String`.
    RestartSession,
    /// Создать новую beads-задачу. `params.title: String, params.priority?: u8`.
    CreateTask,
    /// Открыть проект по id. `params.id: String`.
    OpenProject,
}

impl SystemActionKind {
    /// Human-readable строка (для логов / описаний UI).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenSession => "open_session",
            Self::RestartSession => "restart_session",
            Self::CreateTask => "create_task",
            Self::OpenProject => "open_project",
        }
    }
}

/// Action, который Claude предложил пользователю.
///
/// Wire-формат для парсера — `tag = "kind"`. См. примеры выше в module-doc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Безопасное действие: при клике клиент шлёт `text` как новый
    /// `user_message`. Сервер обрабатывает как обычный prompt.
    Prompt {
        id: String,
        label: String,
        /// Тело сообщения, которое уйдёт в чат. Может содержать markdown.
        text: String,
    },
    /// Системное действие — требует confirmation modal'я на фронтенде и
    /// **отклоняется** при `autonomous_context = true`.
    System {
        id: String,
        label: String,
        name: SystemActionKind,
        #[serde(default)]
        params: serde_json::Value,
    },
}

impl Action {
    pub fn id(&self) -> &str {
        match self {
            Action::Prompt { id, .. } | Action::System { id, .. } => id,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Action::Prompt { label, .. } | Action::System { label, .. } => label,
        }
    }
    /// Превращает в wire-описание для [`crate::ws::protocol::ActionDescriptor`].
    /// UI получает упрощённую структуру (без полного prompt-text'а, чтобы не
    /// разглашать его в notification).
    pub fn to_descriptor(&self) -> crate::ws::protocol::ActionDescriptor {
        match self {
            Action::Prompt { id, label, text } => crate::ws::protocol::ActionDescriptor {
                id: id.clone(),
                label: label.clone(),
                kind: "prompt".into(),
                params: serde_json::json!({ "text": text }),
            },
            Action::System {
                id,
                label,
                name,
                params,
            } => crate::ws::protocol::ActionDescriptor {
                id: id.clone(),
                label: label.clone(),
                kind: name.as_str().into(),
                params: params.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_prompt_round_trip_json() {
        let a = Action::Prompt {
            id: "p1".into(),
            label: "Hi".into(),
            text: "Say hi".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        let parsed: Action = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, a);
        assert!(s.contains("\"kind\":\"prompt\""));
    }

    #[test]
    fn action_system_round_trip_json() {
        let a = Action::System {
            id: "s1".into(),
            label: "Open dev".into(),
            name: SystemActionKind::OpenSession,
            params: serde_json::json!({"name": "dev"}),
        };
        let s = serde_json::to_string(&a).unwrap();
        let parsed: Action = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, a);
        assert!(s.contains("\"kind\":\"system\""));
        assert!(s.contains("\"name\":\"open_session\""));
    }

    #[test]
    fn system_kind_unknown_name_fails_parse() {
        let s = r#"{"kind":"system","id":"x","label":"y","name":"nuclear_launch","params":{}}"#;
        let r: Result<Action, _> = serde_json::from_str(s);
        assert!(r.is_err());
    }

    #[test]
    fn to_descriptor_prompt_carries_text() {
        let a = Action::Prompt {
            id: "p".into(),
            label: "L".into(),
            text: "hello".into(),
        };
        let d = a.to_descriptor();
        assert_eq!(d.kind, "prompt");
        assert_eq!(d.params["text"], "hello");
    }

    #[test]
    fn to_descriptor_system_kind_is_name() {
        let a = Action::System {
            id: "s".into(),
            label: "L".into(),
            name: SystemActionKind::CreateTask,
            params: serde_json::json!({"title": "T"}),
        };
        let d = a.to_descriptor();
        assert_eq!(d.kind, "create_task");
        assert_eq!(d.params["title"], "T");
    }
}
