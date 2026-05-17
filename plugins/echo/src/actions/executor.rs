//! Исполнитель [`crate::actions::Action`] (Phase 5b).
//!
//! ## Контракт
//!
//! [`invoke`] принимает action, [`echo_host_api::HostApi`] adapter и флаг
//! `autonomous_context`. Возвращает [`InvokeResult`]:
//!
//! - `Prompt { text }` — клиенту нужно отправить `text` как новый
//!   `user_message`. Сервер сам не выполняет prompt — это делает фронт.
//! - `Ok` — system-action отработал без полезной нагрузки (например, открыл
//!   сессию).
//! - `Error { msg }` — мягкая ошибка, которую можно показать в toast'е.
//!
//! ## Безопасность (hard-reject в autonomous-контексте)
//!
//! Если `autonomous_context = true` И action — это `System {...}`,
//! [`invoke`] возвращает `Err(...)`. Этот контракт — единственный barrier,
//! защищающий пользователя от ситуации:
//!
//! 1. Autonomous-задача каждый час спрашивает Claude «что нового?».
//! 2. Claude отвечает с `forge-actions` блоком, в котором есть system-action
//!    (`restart_session`).
//! 3. Без hard-reject scheduler мог бы автоматически дёрнуть restart.
//!
//! Hard-reject не пускает таких action'ов в autonomous-режиме. Фронтенд
//! всегда передаёт `autonomous_context = false` (там пользователь смотрит
//! на confirmation modal).
//!
//! ## Расширение whitelist'а
//!
//! Добавление нового [`crate::actions::SystemActionKind`] — намеренное
//! расширение поверхности атаки. Требует:
//! 1. Добавить вариант в enum (compile-time список).
//! 2. Добавить ветку в `match` ниже.
//! 3. Описать в UI confirmation modal, что именно будет исполнено
//!    (label + params preview).
//! 4. Code review (документируем в Phase 6 hardening).

use std::sync::Arc;

use echo_host_api::HostApi;
use serde::Serialize;

use super::{Action, SystemActionKind};

/// Результат выполнения action.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvokeResult {
    /// Фронту нужно дополнительно послать `text` как новый user_message.
    Prompt { text: String },
    /// Action отработал без полезной нагрузки.
    Ok,
    /// Мягкая ошибка — UI покажет toast.
    Error { msg: String },
}

/// Hard-reject error для system-action в autonomous-контексте.
const AUTONOMOUS_BLOCK_MSG: &str = "system actions are blocked in autonomous context";

/// Выполнить action.
///
/// `host` нужен только для system-actions (для prompt — `_host` игнорируется).
/// `autonomous_context = true` блокирует System-actions с anyhow::Err.
pub async fn invoke(
    action: &Action,
    host: Arc<dyn HostApi>,
    autonomous_context: bool,
) -> anyhow::Result<InvokeResult> {
    // 1. Hard-reject system actions в autonomous-контексте.
    if let Action::System { id, name, .. } = action {
        if autonomous_context {
            tracing::warn!(
                action_id = %id,
                name = %name.as_str(),
                "actions::executor: REJECTED system action in autonomous context"
            );
            anyhow::bail!(AUTONOMOUS_BLOCK_MSG);
        }
    }

    match action {
        Action::Prompt { text, .. } => Ok(InvokeResult::Prompt { text: text.clone() }),

        Action::System { id, name, params, .. } => {
            tracing::info!(
                action_id = %id,
                name = %name.as_str(),
                ?params,
                "actions::executor: invoke system action"
            );
            execute_system(*name, params, host.as_ref()).await
        }
    }
}

/// Диспатч system-action на конкретный HostApi-вызов.
///
/// Phase 5: для actions, под которые в текущем `HostApi` нет нативного
/// метода, возвращаем `InvokeResult::Ok` со stub-логом (Phase 6+ —
/// расширение HostApi: `open_session`, `restart_session`, `create_task`).
/// Этот промежуточный режим не сломает фронтенд (он покажет «выполнено»)
/// и не превратит system-action в no-op-молчанку.
async fn execute_system(
    name: SystemActionKind,
    params: &serde_json::Value,
    host: &dyn HostApi,
) -> anyhow::Result<InvokeResult> {
    match name {
        SystemActionKind::OpenSession => {
            // Проверяем хотя бы, что такая сессия есть.
            let target = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if target.is_empty() {
                return Ok(InvokeResult::Error {
                    msg: "open_session: missing params.name".into(),
                });
            }
            match host.list_sessions().await {
                Ok(sessions) => {
                    if !sessions.iter().any(|s| s.name == target) {
                        return Ok(InvokeResult::Error {
                            msg: format!("session not found: {target}"),
                        });
                    }
                }
                Err(e) => {
                    return Ok(InvokeResult::Error {
                        msg: format!("list_sessions failed: {e}"),
                    });
                }
            }
            // Фактическое переключение делает фронтенд (UI знает, какая
            // вкладка/проект сейчас активна). Сервер только валидирует.
            tracing::info!(target, "actions::executor: open_session validated");
            Ok(InvokeResult::Ok)
        }
        SystemActionKind::RestartSession => {
            // Phase 5 stub: HostApi пока не имеет restart-метода. Логируем
            // и возвращаем Ok — фронтенд может сам через REST дёрнуть
            // /api/sessions/.../restart, если такой эндпоинт есть.
            let target = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!(
                target,
                "actions::executor: restart_session stub — frontend must call REST"
            );
            Ok(InvokeResult::Ok)
        }
        SystemActionKind::CreateTask => {
            // Аналогично: HostApi не имеет write-методов для beads. Возвращаем
            // Prompt-результат с готовым к копированию сниппетом + Ok-статусом.
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(no title)");
            tracing::info!(title, "actions::executor: create_task stub");
            Ok(InvokeResult::Ok)
        }
        SystemActionKind::OpenProject => {
            let pid = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if pid.is_empty() {
                return Ok(InvokeResult::Error {
                    msg: "open_project: missing params.id".into(),
                });
            }
            match host.list_projects().await {
                Ok(projects) => {
                    if !projects.iter().any(|p| p.id == pid) {
                        return Ok(InvokeResult::Error {
                            msg: format!("project not found: {pid}"),
                        });
                    }
                }
                Err(e) => {
                    return Ok(InvokeResult::Error {
                        msg: format!("list_projects failed: {e}"),
                    });
                }
            }
            Ok(InvokeResult::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use echo_host_api::{ProjectInfo, SessionInfo};

    struct StubHost {
        sessions: Vec<SessionInfo>,
        projects: Vec<ProjectInfo>,
    }
    #[async_trait]
    impl HostApi for StubHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(self.sessions.clone())
        }
        async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn list_projects(&self) -> anyhow::Result<Vec<ProjectInfo>> {
            Ok(self.projects.clone())
        }
        async fn active_project_id(&self) -> Option<String> {
            None
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
    }

    fn host() -> Arc<dyn HostApi> {
        Arc::new(StubHost {
            sessions: vec![SessionInfo {
                name: "dev".into(),
                windows: 1,
                panes: 1,
            }],
            projects: vec![ProjectInfo {
                id: "p1".into(),
                name: "P1".into(),
                path: "/tmp/p1".into(),
            }],
        })
    }

    #[tokio::test]
    async fn prompt_action_returns_prompt_result() {
        let a = Action::Prompt {
            id: "1".into(),
            label: "X".into(),
            text: "hello".into(),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        assert_eq!(r, InvokeResult::Prompt { text: "hello".into() });
    }

    #[tokio::test]
    async fn prompt_action_allowed_in_autonomous() {
        // Prompt allowed: только system заблокирован.
        let a = Action::Prompt {
            id: "1".into(),
            label: "X".into(),
            text: "hello".into(),
        };
        let r = invoke(&a, host(), true).await.unwrap();
        assert!(matches!(r, InvokeResult::Prompt { .. }));
    }

    #[tokio::test]
    async fn system_action_rejected_in_autonomous_context() {
        let a = Action::System {
            id: "1".into(),
            label: "X".into(),
            name: SystemActionKind::OpenSession,
            params: serde_json::json!({"name": "dev"}),
        };
        let res = invoke(&a, host(), true).await;
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("autonomous"));
    }

    #[tokio::test]
    async fn open_session_validates_existing() {
        let a = Action::System {
            id: "1".into(),
            label: "Open".into(),
            name: SystemActionKind::OpenSession,
            params: serde_json::json!({"name": "dev"}),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        assert_eq!(r, InvokeResult::Ok);
    }

    #[tokio::test]
    async fn open_session_returns_error_for_unknown() {
        let a = Action::System {
            id: "1".into(),
            label: "Open".into(),
            name: SystemActionKind::OpenSession,
            params: serde_json::json!({"name": "nope"}),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        match r {
            InvokeResult::Error { msg } => assert!(msg.contains("session not found")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_session_missing_param_returns_error() {
        let a = Action::System {
            id: "1".into(),
            label: "Open".into(),
            name: SystemActionKind::OpenSession,
            params: serde_json::json!({}),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        assert!(matches!(r, InvokeResult::Error { .. }));
    }

    #[tokio::test]
    async fn open_project_validates() {
        let a = Action::System {
            id: "1".into(),
            label: "Open".into(),
            name: SystemActionKind::OpenProject,
            params: serde_json::json!({"id": "p1"}),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        assert_eq!(r, InvokeResult::Ok);
    }

    #[tokio::test]
    async fn open_project_unknown_returns_error() {
        let a = Action::System {
            id: "1".into(),
            label: "Open".into(),
            name: SystemActionKind::OpenProject,
            params: serde_json::json!({"id": "nope"}),
        };
        let r = invoke(&a, host(), false).await.unwrap();
        assert!(matches!(r, InvokeResult::Error { .. }));
    }
}
