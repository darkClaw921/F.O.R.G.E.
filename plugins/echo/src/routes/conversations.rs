//! REST API для chat_sessions + messages.
//!
//! - `GET    /api/echo/conversations?project_id=&limit=` — список.
//! - `POST   /api/echo/conversations` body: `{ title, project_id?, model? }`.
//!   Если `project_id` указан но не найден в host-`ProjectStore` — мы
//!   ЛОГИРУЕМ warning, но НЕ блокируем (soft-FK, как в плане ko8.9).
//! - `DELETE /api/echo/conversations/:id` — каскадно удаляет messages.
//! - `GET    /api/echo/conversations/:id/messages?limit=&before=` — листинг
//!   сообщений ASC по `created_at`.
//!
//! `model` default = `"sonnet-4"`. `limit` default = `50` для list и
//! `200` для messages.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::db::repo::{chats, messages};
use crate::state::EchoState;

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route(
            "/api/echo/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/api/echo/conversations/:id",
            axum::routing::delete(delete_conversation),
        )
        .route(
            "/api/echo/conversations/:id/messages",
            get(list_messages_for_session),
        )
        // POST вариант delete для клиентов без DELETE-метода.
        .route(
            "/api/echo/conversations/:id/delete",
            post(delete_conversation),
        )
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    project_id: Option<String>,
    #[serde(default = "default_list_limit")]
    limit: i64,
}
fn default_list_limit() -> i64 {
    50
}

async fn list_conversations(
    State(state): State<Arc<EchoState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let items = chats::list(&state.db, q.project_id.as_deref(), q.limit)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    title: String,
    project_id: Option<String>,
    model: Option<String>,
}

async fn create_conversation(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<CreateBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // После Phase 4 (`remove-projects-concept`) HostApi больше не
    // позволяет перечислить проекты, поэтому soft-FK-валидация
    // невозможна. `project_id` остаётся как непрозрачный label —
    // принимаем любое значение, ответственность за смысл лежит на caller'е.
    let model = b.model.as_deref().unwrap_or("sonnet-4");
    let s = chats::create(&state.db, &b.title, b.project_id.as_deref(), model)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(s).unwrap()))
}

async fn delete_conversation(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // Не различаем «было/не было» — DELETE идемпотентен.
    chats::delete(&state.db, &id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct MessagesQuery {
    #[serde(default = "default_msg_limit")]
    limit: i64,
    before: Option<i64>,
}
fn default_msg_limit() -> i64 {
    200
}

async fn list_messages_for_session(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 404 если самой сессии нет (UX лучше, чем пустой items=[]).
    if chats::get(&state.db, &id).await.map_err(internal)?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "conversation not found".into()));
    }
    let items = messages::list_by_session(&state.db, &id, q.limit, q.before)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo conversations route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
