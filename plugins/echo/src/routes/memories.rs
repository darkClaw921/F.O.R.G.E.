//! REST API для memories.
//!
//! - `GET    /api/echo/memories?scope=&project_id=&day=`
//! - `POST   /api/echo/memories` body: `{ scope, project_id?, day?, content, source? }`
//!   — upsert; всегда 200 OK (UNIQUE-конфликта быть не может).
//! - `PATCH  /api/echo/memories/:id` body: `{ content }` — 404 если нет.
//! - `DELETE /api/echo/memories/:id` — 404 если нет.
//!
//! Ошибки сериализуются как `{ "error": "..." }` со статусом 400/404/500.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;

use crate::db::repo::memories::{self, MemoryScope};
use crate::state::EchoState;

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route("/api/echo/memories", get(list).post(upsert))
        .route(
            "/api/echo/memories/:id",
            patch(patch_one).delete(delete_one),
        )
        // Дублирующие explicit-routes для tooling, который не умеет в
        // `MethodRouter` chain (curl всё умеет, но пусть будет явно).
        .route("/api/echo/memories/by-id/:id", get(get_one))
        .route("/api/echo/memories/:id/patch", post(patch_one_post))
        .route("/api/echo/memories/:id/delete", delete(delete_one))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    scope: Option<String>,
    project_id: Option<String>,
    day: Option<String>,
}

async fn list(
    State(state): State<Arc<EchoState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = match q.scope.as_deref() {
        Some(s) => Some(MemoryScope::from_str(s).ok_or_else(|| {
            ApiError(StatusCode::BAD_REQUEST, format!("invalid scope: {s}"))
        })?),
        None => None,
    };
    let items = memories::list(
        &state.db,
        scope,
        q.project_id.as_deref(),
        q.day.as_deref(),
    )
    .await
    .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct UpsertBody {
    scope: String,
    project_id: Option<String>,
    day: Option<String>,
    content: String,
    #[serde(default = "default_source")]
    source: String,
}
fn default_source() -> String {
    "manual".to_string()
}

async fn upsert(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<UpsertBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = MemoryScope::from_str(&b.scope).ok_or_else(|| {
        ApiError(StatusCode::BAD_REQUEST, format!("invalid scope: {}", b.scope))
    })?;
    let m = memories::upsert(
        &state.db,
        scope,
        b.project_id.as_deref(),
        b.day.as_deref(),
        &b.content,
        &b.source,
    )
    .await
    .map_err(internal)?;
    Ok(Json(serde_json::to_value(m).unwrap()))
}

async fn get_one(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let m = memories::get(&state.db, &id).await.map_err(internal)?;
    match m {
        Some(m) => Ok(Json(serde_json::to_value(m).unwrap())),
        None => Err(ApiError(StatusCode::NOT_FOUND, "not found".into())),
    }
}

#[derive(Debug, Deserialize)]
struct PatchBody {
    content: String,
}

async fn patch_one(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> Result<StatusCode, ApiError> {
    let n = memories::patch(&state.db, &id, &b.content)
        .await
        .map_err(internal)?;
    if n == 0 {
        Err(ApiError(StatusCode::NOT_FOUND, "not found".into()))
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn patch_one_post(
    state: State<Arc<EchoState>>,
    path: Path<String>,
    body: Json<PatchBody>,
) -> Result<StatusCode, ApiError> {
    patch_one(state, path, body).await
}

async fn delete_one(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let n = memories::delete(&state.db, &id).await.map_err(internal)?;
    if n == 0 {
        Err(ApiError(StatusCode::NOT_FOUND, "not found".into()))
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// Простая JSON-ошибка с произвольным статусом.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo memories route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
