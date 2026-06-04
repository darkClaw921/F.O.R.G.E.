//! REST API фичи «Следующий шаг» (`next-steps`).
//!
//! - `GET  /api/echo/next-steps` → `{ items: [{ session, content, created_at }] }`
//!   — текущие эфемерные предложения из
//!   [`EchoState::next_steps`](crate::state::EchoState::next_steps).
//! - `POST /api/echo/next-steps/:session/send` body `{ text }`
//!   → [`HostApi::send_keys`] доставляет текст в сессию, затем предложение
//!   снимается из `next_steps` + broadcast `NextStepEvent{has_suggestion:false}`.
//!   Если `text` пуст — берётся `content` сохранённого предложения.
//! - `POST /api/echo/next-steps/:session/feedback` body `{ correction }`
//!   → пишет правило в `next_step_rules`
//!   (`context_summary` = `pane_excerpt` + отвергнутое предложение,
//!   `suggested_next` = `correction`), снимает предложение + broadcast.
//! - `POST /api/echo/next-steps/:session/dismiss`
//!   → просто снимает предложение из `next_steps` + broadcast.
//!
//! Ошибки сериализуются как `{ "error": "..." }` (та же конвенция [`ApiError`],
//! что и в [`crate::routes::daily_reports`]). 404 — если для сессии нет
//! активного предложения там, где оно требуется.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::db::repo::next_step as rules_repo;
use crate::state::{EchoState, ServerEvent};
use crate::ws::protocol::ServerMsg;

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route("/api/echo/next-steps", get(list))
        .route("/api/echo/next-steps/:session/send", post(send))
        .route("/api/echo/next-steps/:session/feedback", post(feedback))
        .route("/api/echo/next-steps/:session/dismiss", post(dismiss))
}

/// `GET /api/echo/next-steps` → текущие предложения.
async fn list(State(state): State<Arc<EchoState>>) -> Json<serde_json::Value> {
    let map = state.next_steps.read().await;
    let items: Vec<serde_json::Value> = map
        .values()
        .map(|s| {
            serde_json::json!({
                "session": s.session,
                "content": s.content,
                "created_at": s.created_at_unix,
            })
        })
        .collect();
    Json(serde_json::json!({ "items": items }))
}

/// Шлёт broadcast `NextStepEvent{has_suggestion:false}` для снятого
/// предложения (общий хвост send/feedback/dismiss).
fn broadcast_cleared(state: &Arc<EchoState>, session: &str) {
    let _ = state.broadcast.send(ServerEvent::broadcast(ServerMsg::NextStepEvent {
        session: session.to_string(),
        has_suggestion: false,
    }));
}

#[derive(Debug, Deserialize)]
struct SendBody {
    #[serde(default)]
    text: Option<String>,
}

/// `POST /api/echo/next-steps/:session/send` — доставить шаг в сессию.
///
/// `text` опционален: при отсутствии/пустоте берётся `content` сохранённого
/// предложения. 404 если предложения нет и `text` не задан.
async fn send(
    State(state): State<Arc<EchoState>>,
    Path(session): Path<String>,
    Json(b): Json<SendBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Снимаем предложение (если есть) и определяем итоговый текст.
    let suggestion = state.next_steps.write().await.remove(&session);
    let text = match b.text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => suggestion
            .as_ref()
            .map(|s| s.content.clone())
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("no suggestion to send for session {session}"),
                )
            })?,
    };

    let host = host(&state)?;
    host.send_keys(&session, &text).await.map_err(internal)?;

    broadcast_cleared(&state, &session);
    Ok(Json(serde_json::json!({ "ok": true, "sent": text })))
}

#[derive(Debug, Deserialize)]
struct FeedbackBody {
    correction: String,
}

/// `POST /api/echo/next-steps/:session/feedback` — записать правило памяти из
/// коррекции пользователя и снять предложение.
async fn feedback(
    State(state): State<Arc<EchoState>>,
    Path(session): Path<String>,
    Json(b): Json<FeedbackBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let correction = b.correction.trim().to_string();
    if correction.is_empty() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "correction must not be empty".into(),
        ));
    }

    let suggestion = state.next_steps.write().await.remove(&session);

    // context_summary = pane-выдержка + отвергнутое предложение (если было).
    let (context_summary, project_id) = match &suggestion {
        Some(s) => (
            format!(
                "Контекст терминала:\n{}\n\nОтвергнутое предложение: {}",
                s.pane_excerpt, s.content
            ),
            s.project_id.clone(),
        ),
        None => (
            format!("Сессия: {session} (предложение уже снято)"),
            None,
        ),
    };

    let rule = rules_repo::insert_rule(
        &state.db,
        project_id.as_deref(),
        &context_summary,
        &correction,
    )
    .await
    .map_err(internal)?;

    broadcast_cleared(&state, &session);
    Ok(Json(serde_json::json!({ "ok": true, "rule_id": rule.id })))
}

/// `POST /api/echo/next-steps/:session/dismiss` — снять предложение без действия.
async fn dismiss(
    State(state): State<Arc<EchoState>>,
    Path(session): Path<String>,
) -> Json<serde_json::Value> {
    let removed = state.next_steps.write().await.remove(&session).is_some();
    if removed {
        broadcast_cleared(&state, &session);
    }
    Json(serde_json::json!({ "ok": true, "dismissed": removed }))
}

/// Достаёт host-adapter из state или 500.
fn host(state: &Arc<EchoState>) -> Result<Arc<dyn crate::HostApi>, ApiError> {
    state.host.get().cloned().ok_or_else(|| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "host adapter missing".into(),
        )
    })
}

/// Простая JSON-ошибка с произвольным статусом (та же конвенция, что и в
/// [`crate::routes::daily_reports`]).
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo next_step route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use crate::routes::build_router;
    use crate::state::NextStepSuggestion;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use echo_host_api::{HostApi, SessionInfo};
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;

    /// Stub-host, фиксирующий последний send_keys-вызов.
    struct StubHost {
        sent: StdMutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl HostApi for StubHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(Vec::new())
        }
        async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
            Ok(String::new())
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
        async fn send_keys(&self, session: &str, text: &str) -> anyhow::Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push((session.to_string(), text.to_string()));
            Ok(())
        }
    }

    async fn make_state() -> (Arc<EchoState>, Arc<StubHost>) {
        let runner = Arc::new(ClaudeRunner::new(std::path::PathBuf::from("/nope"), 1));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let stub = Arc::new(StubHost {
            sent: StdMutex::new(Vec::new()),
        });
        let host: Arc<dyn HostApi> = stub.clone();
        state.host.set(host).ok();
        (state, stub)
    }

    async fn seed_suggestion(state: &Arc<EchoState>, session: &str) {
        state.next_steps.write().await.insert(
            session.to_string(),
            NextStepSuggestion {
                session: session.to_string(),
                content: "сделай X".into(),
                pane_excerpt: "tail of pane".into(),
                project_id: None,
                created_at_unix: 1000,
            },
        );
    }

    async fn req_json(
        state: Arc<EchoState>,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let app = build_router(state);
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                builder = builder.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn list_returns_current_suggestions() {
        let (state, _) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, v) = req_json(state, "GET", "/api/echo/next-steps", None).await;
        assert_eq!(status, StatusCode::OK);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["session"], "work");
        assert_eq!(items[0]["content"], "сделай X");
        assert_eq!(items[0]["created_at"], 1000);
    }

    #[tokio::test]
    async fn send_uses_suggestion_content_and_clears() {
        let (state, stub) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, v) = req_json(
            state.clone(),
            "POST",
            "/api/echo/next-steps/work/send",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], true);
        assert_eq!(v["sent"], "сделай X");
        // send_keys вызван с content предложения.
        let sent = stub.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], ("work".to_string(), "сделай X".to_string()));
        // Предложение снято.
        assert!(state.next_steps.read().await.is_empty());
    }

    #[tokio::test]
    async fn send_with_explicit_text_overrides() {
        let (state, stub) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, _) = req_json(
            state.clone(),
            "POST",
            "/api/echo/next-steps/work/send",
            Some(serde_json::json!({ "text": "другой шаг" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let sent = stub.sent.lock().unwrap();
        assert_eq!(sent[0].1, "другой шаг");
    }

    #[tokio::test]
    async fn send_without_suggestion_or_text_is_404() {
        let (state, _) = make_state().await;
        let (status, _) = req_json(
            state,
            "POST",
            "/api/echo/next-steps/ghost/send",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn feedback_writes_rule_and_clears() {
        let (state, _) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, v) = req_json(
            state.clone(),
            "POST",
            "/api/echo/next-steps/work/feedback",
            Some(serde_json::json!({ "correction": "лучше запусти тесты" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], true);
        assert!(v["rule_id"].as_str().unwrap().len() > 0);
        // Предложение снято.
        assert!(state.next_steps.read().await.is_empty());
        // Правило записано.
        let rules = rules_repo::list_rules(&state.db, None, 20).await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].suggested_next, "лучше запусти тесты");
        assert!(rules[0].context_summary.contains("tail of pane"));
        assert!(rules[0].context_summary.contains("сделай X"));
    }

    #[tokio::test]
    async fn feedback_empty_correction_is_400() {
        let (state, _) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, _) = req_json(
            state,
            "POST",
            "/api/echo/next-steps/work/feedback",
            Some(serde_json::json!({ "correction": "   " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dismiss_clears_suggestion() {
        let (state, _) = make_state().await;
        seed_suggestion(&state, "work").await;
        let (status, v) = req_json(
            state.clone(),
            "POST",
            "/api/echo/next-steps/work/dismiss",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["dismissed"], true);
        assert!(state.next_steps.read().await.is_empty());
    }
}
