//! Wire-протокол WebSocket `/ws/echo`.
//!
//! Serde-tagged enums с `tag = "type"` и `rename_all = "snake_case"`.
//! Это даёт стабильный JSON-формат без зависимости от названий вариантов
//! Rust-кода. Изменение варианта enum → breaking change для фронтенда; для
//! защиты от такого регресса есть round-trip тесты.
//!
//! ## ClientMsg → ServerMsg маршруты (определяются в `ws/mod.rs`)
//!
//! - `user_message`  → инсёрт user-msg в db → ClaudeRunner stream →
//!                     `assistant_chunk` (много) → `assistant_done`.
//! - `cancel`        → ClaudeRunner::cancel(run_id) → стрим обрывается.
//! - `action_invoke` → actions::executor::invoke (Phase 5; в P3 — error/ignore).
//! - `pong`          → reset heartbeat-deadline.
//!
//! ## Серверные события
//!
//! - `assistant_chunk`        — частичное приращение (text/thinking/tool).
//! - `assistant_done`         — финал одного assistant-ответа + usage.
//! - `action_buttons`         — список кнопок-действий (Phase 5 stub).
//! - `notification`           — toast-уведомление (Phase 5 stub).
//! - `stats_update`           — апдейт sparkline по minute-bucket'у.
//! - `autonomous_task_event`  — события scheduler'а (Phase 4 stub).
//! - `error`                  — серверная ошибка (например, run failed).
//! - `ping`                   — heartbeat (раз в 15с; клиент отвечает `pong`).

use serde::{Deserialize, Serialize};

use crate::claude::events::Usage;

/// Сообщения клиент → сервер.
///
/// `#[serde(tag = "type", rename_all = "snake_case")]` даёт wire-формат
/// вида `{"type":"user_message", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Юзер послал сообщение в чат.
    UserMessage {
        text: String,
        conversation_id: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        ctx_opts: Option<CtxOptsWire>,
    },
    /// Прервать запущенный run.
    Cancel { run_id: String },
    /// Инвокать action-кнопку (Phase 5; в P3 — пока stub).
    ActionInvoke {
        action_id: String,
        #[serde(default)]
        params: serde_json::Value,
    },
    /// Ответ на серверный `ping`.
    Pong,
}

/// Контекстные опции для prompt-builder'а, передаваемые в `user_message`.
///
/// Сериализация — `rename_all = "snake_case"` чтобы совпадать с фронтендом.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CtxOptsWire {
    #[serde(default = "default_include_pane")]
    pub include_pane_capture: bool,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default = "default_include_mem")]
    pub include_memories: bool,
    #[serde(default = "default_capture_lines")]
    pub capture_lines: i32,
    #[serde(default)]
    pub session_filter: Option<Vec<String>>,
}

fn default_include_pane() -> bool {
    true
}
fn default_include_mem() -> bool {
    true
}
fn default_capture_lines() -> i32 {
    200
}

/// Сообщения сервер → клиент.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Очередной кусок assistant-ответа.
    AssistantChunk {
        run_id: String,
        kind: ChunkKind,
        delta: String,
    },
    /// Финал одного assistant-ответа.
    AssistantDone {
        run_id: String,
        usage: Usage,
        message_id: String,
    },
    /// Phase 5 — список action-кнопок после ответа. В P3 не emit'ится.
    ActionButtons {
        message_id: String,
        actions: Vec<ActionDescriptor>,
    },
    /// Phase 5 — toast. В P3 не emit'ится.
    Notification {
        level: NotificationLevel,
        title: String,
        body: String,
    },
    /// Realtime-апдейт sparkline'а (per-minute bucket).
    StatsUpdate {
        tokens_in_per_min: u64,
        tokens_out_per_min: u64,
    },
    /// Phase 4 — события scheduler'а autonomous-задач. В P3 не emit'ится.
    AutonomousTaskEvent {
        task_id: String,
        run_id: String,
        status: String,
        #[serde(default)]
        message_preview: Option<String>,
    },
    /// Серверная ошибка (например, run failed, model unavailable).
    Error { code: String, message: String },
    /// Heartbeat-ping; клиент должен ответить `pong`.
    Ping,
    /// Фича «Следующий шаг» — изменилось состояние предложения для сессии.
    ///
    /// Broadcast всем клиентам (без `conversation_id`). `has_suggestion`:
    /// - `true`  — для сессии появилось новое предложение (воркер сгенерировал);
    /// - `false` — предложение снято (отправлено / dismiss / feedback / сессия
    ///   снова активна).
    ///
    /// Фронтенд по этому событию перефетчит `GET /api/echo/next-steps` и
    /// обновит голубое свечение/попап у соответствующей сессии.
    NextStepEvent {
        session: String,
        has_suggestion: bool,
    },
}

/// Тип чанка для assistant_chunk. Сериализуется как snake_case ("text",
/// "thinking", "tool_use").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Text,
    Thinking,
    ToolUse,
}

/// Уровень уведомления для toast'ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Warn,
    Error,
}

/// Описание action-кнопки (Phase 5). В P3 — определена только структура для
/// совместимости (структура импортируется фронтендом сразу).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String, // "send_keys" | "create_task" | "open_url" | "open_session" ...
    #[serde(default)]
    pub params: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_client(msg: &ClientMsg) -> serde_json::Value {
        let s = serde_json::to_string(msg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("re-parse");
        // round-trip: ClientMsg -> JSON -> ClientMsg → still parses
        let _: ClientMsg = serde_json::from_str(&s).expect("round-trip parse");
        v
    }

    fn rt_server(msg: &ServerMsg) -> serde_json::Value {
        let s = serde_json::to_string(msg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("re-parse");
        let _: ServerMsg = serde_json::from_str(&s).expect("round-trip parse");
        v
    }

    #[test]
    fn client_user_message_uses_snake_case_tag() {
        let m = ClientMsg::UserMessage {
            text: "hi".into(),
            conversation_id: "c1".into(),
            model: Some("sonnet".into()),
            ctx_opts: None,
        };
        let v = rt_client(&m);
        assert_eq!(v["type"], "user_message");
        assert_eq!(v["text"], "hi");
        assert_eq!(v["conversation_id"], "c1");
        assert_eq!(v["model"], "sonnet");
    }

    #[test]
    fn client_cancel_round_trip() {
        let m = ClientMsg::Cancel { run_id: "r1".into() };
        let v = rt_client(&m);
        assert_eq!(v["type"], "cancel");
        assert_eq!(v["run_id"], "r1");
    }

    #[test]
    fn client_action_invoke_default_params_is_null() {
        let s = r#"{"type":"action_invoke","action_id":"a1"}"#;
        let m: ClientMsg = serde_json::from_str(s).expect("parse");
        match m {
            ClientMsg::ActionInvoke { action_id, params } => {
                assert_eq!(action_id, "a1");
                assert!(params.is_null());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn client_pong_minimal() {
        let s = r#"{"type":"pong"}"#;
        let m: ClientMsg = serde_json::from_str(s).expect("parse");
        assert!(matches!(m, ClientMsg::Pong));
    }

    #[test]
    fn server_assistant_chunk_text_kind() {
        let m = ServerMsg::AssistantChunk {
            run_id: "r1".into(),
            kind: ChunkKind::Text,
            delta: "Hi".into(),
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "assistant_chunk");
        assert_eq!(v["kind"], "text");
        assert_eq!(v["delta"], "Hi");
    }

    #[test]
    fn server_assistant_chunk_thinking_and_tool_use_kinds() {
        for (k, expected) in [
            (ChunkKind::Thinking, "thinking"),
            (ChunkKind::ToolUse, "tool_use"),
        ] {
            let m = ServerMsg::AssistantChunk {
                run_id: "r".into(),
                kind: k,
                delta: "d".into(),
            };
            let v = rt_server(&m);
            assert_eq!(v["kind"], expected);
        }
    }

    #[test]
    fn server_assistant_done_carries_usage() {
        let m = ServerMsg::AssistantDone {
            run_id: "r1".into(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            message_id: "m1".into(),
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "assistant_done");
        assert_eq!(v["usage"]["input_tokens"], 10);
        assert_eq!(v["usage"]["output_tokens"], 5);
        assert_eq!(v["message_id"], "m1");
    }

    #[test]
    fn server_stats_update() {
        let m = ServerMsg::StatsUpdate {
            tokens_in_per_min: 42,
            tokens_out_per_min: 7,
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "stats_update");
        assert_eq!(v["tokens_in_per_min"], 42);
        assert_eq!(v["tokens_out_per_min"], 7);
    }

    #[test]
    fn server_error_and_ping() {
        let e = ServerMsg::Error {
            code: "run_failed".into(),
            message: "boom".into(),
        };
        let v = rt_server(&e);
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "run_failed");

        let p = ServerMsg::Ping;
        let v = rt_server(&p);
        assert_eq!(v["type"], "ping");
    }

    #[test]
    fn server_action_buttons_round_trip() {
        let m = ServerMsg::ActionButtons {
            message_id: "m1".into(),
            actions: vec![ActionDescriptor {
                id: "a".into(),
                label: "Send".into(),
                kind: "send_keys".into(),
                params: serde_json::json!({"text": "ls"}),
            }],
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "action_buttons");
        assert_eq!(v["actions"][0]["type"], "send_keys");
        assert_eq!(v["actions"][0]["params"]["text"], "ls");
    }

    #[test]
    fn server_notification_levels() {
        for (lvl, expected) in [
            (NotificationLevel::Info, "info"),
            (NotificationLevel::Warn, "warn"),
            (NotificationLevel::Error, "error"),
        ] {
            let m = ServerMsg::Notification {
                level: lvl,
                title: "t".into(),
                body: "b".into(),
            };
            let v = rt_server(&m);
            assert_eq!(v["level"], expected);
        }
    }

    #[test]
    fn server_autonomous_task_event_round_trip() {
        let m = ServerMsg::AutonomousTaskEvent {
            task_id: "t1".into(),
            run_id: "r1".into(),
            status: "running".into(),
            message_preview: Some("doing X".into()),
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "autonomous_task_event");
        assert_eq!(v["task_id"], "t1");
        assert_eq!(v["status"], "running");
        assert_eq!(v["message_preview"], "doing X");
    }

    #[test]
    fn server_next_step_event_round_trip() {
        let m = ServerMsg::NextStepEvent {
            session: "work".into(),
            has_suggestion: true,
        };
        let v = rt_server(&m);
        assert_eq!(v["type"], "next_step_event");
        assert_eq!(v["session"], "work");
        assert_eq!(v["has_suggestion"], true);

        let cleared = ServerMsg::NextStepEvent {
            session: "work".into(),
            has_suggestion: false,
        };
        let v = rt_server(&cleared);
        assert_eq!(v["has_suggestion"], false);
    }

    #[test]
    fn ctx_opts_defaults_when_absent_from_user_message() {
        let s = r#"{"type":"user_message","text":"hi","conversation_id":"c","ctx_opts":{}}"#;
        let m: ClientMsg = serde_json::from_str(s).expect("parse");
        if let ClientMsg::UserMessage { ctx_opts, .. } = m {
            let o = ctx_opts.expect("present");
            assert!(o.include_pane_capture);
            assert!(o.include_memories);
            assert_eq!(o.capture_lines, 200);
        } else {
            panic!("expected user_message");
        }
    }
}
