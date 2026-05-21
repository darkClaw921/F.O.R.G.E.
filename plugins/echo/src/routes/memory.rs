//! REST API для memory automation.
//!
//! Phase 5a — POST `/api/echo/memories/regenerate` запускает суммаризацию
//! «по требованию»:
//!
//! - `scope=global_day` + `day=YYYY-MM-DD` → `memory::summarize_day(.., None)`
//! - `scope=project_day` + `project_id=...` + `day=YYYY-MM-DD`
//!     → `memory::summarize_day(.., Some(pid))`
//! - `scope=project` + `project_id=...` → `memory::summarize_project(.., pid)`
//!
//! Запуск синхронный с таймаутом 90 секунд (CLI обычно укладывается за
//! 10-20с). При таймауте возвращаем 504. При invalid scope/missing field —
//! 400. При успехе — 200 + `{ memory_id, scope, day?, project_id? }`.
//!
//! Файл namespaced как `memory.rs` (не `memories.rs`) чтобы не конфликтовать
//! с уже существующим `routes/memories.rs` (CRUD-роуты Phase 2). Внутри
//! одного `Router` оба сосуществуют благодаря разным path'ам.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;

use crate::db::repo::memories::MemoryScope;
use crate::memory;
use crate::state::EchoState;

/// Таймаут на синхронный регенератор. CLI Claude обычно отвечает 10-20с;
/// 90 даёт запас под медленные сети/большие prompt'ы. Если не успеваем —
/// 504, фронтенд может повторить или подождать background-rollover.
const REGENERATE_TIMEOUT: Duration = Duration::from_secs(90);

pub fn router() -> Router<Arc<EchoState>> {
    Router::new().route("/api/echo/memories/regenerate", post(regenerate))
}

#[derive(Debug, Deserialize)]
struct RegenerateBody {
    scope: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    day: Option<String>,
}

async fn regenerate(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<RegenerateBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = MemoryScope::from_str(&b.scope).ok_or_else(|| {
        ApiError(
            StatusCode::BAD_REQUEST,
            format!("invalid scope: {}", b.scope),
        )
    })?;

    let host = state
        .host
        .get()
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "host adapter missing".into()))?;

    let fut = async move {
        match scope {
            MemoryScope::GlobalDay => {
                let day_str = b.day.ok_or_else(|| {
                    ApiError(StatusCode::BAD_REQUEST, "day required for scope=global_day".into())
                })?;
                let day = chrono::NaiveDate::parse_from_str(&day_str, "%Y-%m-%d").map_err(|e| {
                    ApiError(StatusCode::BAD_REQUEST, format!("invalid day: {e}"))
                })?;
                let id = memory::summarize_day(state.clone(), host, day, None)
                    .await
                    .map_err(internal)?;
                Ok::<_, ApiError>(serde_json::json!({
                    "memory_id": id,
                    "scope": "global_day",
                    "day": day_str,
                }))
            }
            MemoryScope::ProjectDay => {
                let pid = b.project_id.ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        "project_id required for scope=project_day".into(),
                    )
                })?;
                let day_str = b.day.ok_or_else(|| {
                    ApiError(StatusCode::BAD_REQUEST, "day required for scope=project_day".into())
                })?;
                let day = chrono::NaiveDate::parse_from_str(&day_str, "%Y-%m-%d").map_err(|e| {
                    ApiError(StatusCode::BAD_REQUEST, format!("invalid day: {e}"))
                })?;
                let id = memory::summarize_day(state.clone(), host, day, Some(&pid))
                    .await
                    .map_err(internal)?;
                Ok(serde_json::json!({
                    "memory_id": id,
                    "scope": "project_day",
                    "project_id": pid,
                    "day": day_str,
                }))
            }
            MemoryScope::Project => {
                let pid = b.project_id.ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        "project_id required for scope=project".into(),
                    )
                })?;
                let id = memory::summarize_project(state.clone(), host, &pid)
                    .await
                    .map_err(internal)?;
                Ok(serde_json::json!({
                    "memory_id": id,
                    "scope": "project",
                    "project_id": pid,
                }))
            }
        }
    };

    match tokio::time::timeout(REGENERATE_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(Json(v)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            format!(
                "regenerate exceeded {}s timeout",
                REGENERATE_TIMEOUT.as_secs()
            ),
        )),
    }
}

/// Простая JSON-ошибка с произвольным статусом (та же конвенция, что и
/// в [`crate::routes::memories`]).
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo memory route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use crate::routes::build_router;
    use crate::state::EchoState;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use echo_host_api::{HostApi, SessionInfo};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tower::ServiceExt;

    struct StubHost;
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
    }

    fn write_mock_cli(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn mock_script() -> &'static str {
        r###"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Auto summary text"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":2,"output_tokens":2}}'
"###
    }

    async fn make_app() -> Arc<EchoState> {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_script());
        std::mem::forget(dir);
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host).ok();
        state
    }

    #[tokio::test]
    async fn regenerate_invalid_scope_returns_400() {
        let state = make_app().await;
        let app = build_router(state.clone());
        let body = serde_json::json!({ "scope": "nonsense" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/memories/regenerate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn regenerate_global_day_missing_day_returns_400() {
        let state = make_app().await;
        let app = build_router(state);
        let body = serde_json::json!({ "scope": "global_day" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/memories/regenerate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn regenerate_global_day_success() {
        let state = make_app().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "scope": "global_day",
            "day": "2026-05-16",
        })
        .to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/memories/regenerate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["scope"], "global_day");
        assert_eq!(v["day"], "2026-05-16");
        assert!(v["memory_id"].as_str().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn regenerate_project_day_requires_pid_and_day() {
        let state = make_app().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "scope": "project_day",
            "day": "2026-05-16",
        })
        .to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/memories/regenerate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn regenerate_project_success() {
        let state = make_app().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "scope": "project",
            "project_id": "p1",
        })
        .to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/memories/regenerate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
