//! REST API для управления автономными задачами Echo.
//!
//! Все handler'ы тонкие — основная логика в [`crate::db::repo::autonomous`]
//! и [`crate::scheduler::runner`].
//!
//! Endpoints (см. план Phase 4):
//!
//! - `GET    /api/echo/autonomous-tasks`            — список всех задач.
//! - `POST   /api/echo/autonomous-tasks`            — создать (+ установить
//!                                                    `next_run_at = now + interval`).
//! - `PATCH  /api/echo/autonomous-tasks/:id`        — частичный апдейт;
//!                                                    при `enabled=true` и
//!                                                    `next_run_at=NULL`
//!                                                    ставит `next_run_at=now`.
//! - `DELETE /api/echo/autonomous-tasks/:id`        — удалить (cascade
//!                                                    сносит `task_runs`).
//! - `POST   /api/echo/autonomous-tasks/:id/run-now` — немедленный запуск
//!                                                    через
//!                                                    [`crate::scheduler::runner::run_task`]
//!                                                    (без ожидания tick'а).
//! - `GET    /api/echo/autonomous-tasks/:id/runs?limit=` — история запусков.
//!
//! Response format везде JSON. Error → `{"error": "<msg>"}` + соответствующий
//! HTTP-статус.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::db::repo::autonomous::{self, TaskPatch};
use crate::scheduler::runner;
use crate::state::EchoState;

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route(
            "/api/echo/autonomous-tasks",
            get(list_tasks).post(create_task),
        )
        .route(
            "/api/echo/autonomous-tasks/:id",
            axum::routing::patch(patch_task).delete(delete_task),
        )
        .route(
            "/api/echo/autonomous-tasks/:id/run-now",
            post(run_now),
        )
        .route(
            "/api/echo/autonomous-tasks/:id/runs",
            get(list_runs),
        )
}

// ────────── List ──────────

async fn list_tasks(
    State(state): State<Arc<EchoState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let items = autonomous::list_tasks(&state.db, false)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ────────── Create ──────────

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: String,
    prompt_template: String,
    interval_seconds: i64,
    model: String,
    #[serde(default)]
    project_id: Option<String>,
}

async fn create_task(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if b.interval_seconds < 1 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "interval_seconds must be >= 1".into(),
        ));
    }
    if b.name.trim().is_empty() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "name must not be empty".into(),
        ));
    }
    let t = autonomous::create_task(
        &state.db,
        &b.name,
        &b.prompt_template,
        b.interval_seconds,
        &b.model,
        b.project_id.as_deref(),
    )
    .await
    .map_err(internal)?;
    // create_task уже ставит next_run_at = now + interval_seconds внутри
    // (см. db/repo/autonomous.rs::create_task), отдельный set_next_run не
    // требуется.
    Ok((StatusCode::CREATED, Json(serde_json::to_value(t).unwrap())))
}

// ────────── Patch ──────────

#[derive(Debug, Deserialize)]
struct PatchBody {
    name: Option<String>,
    prompt_template: Option<String>,
    interval_seconds: Option<i64>,
    model: Option<String>,
    enabled: Option<bool>,
}

async fn patch_task(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(iv) = b.interval_seconds {
        if iv < 1 {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "interval_seconds must be >= 1".into(),
            ));
        }
    }
    if autonomous::get_task(&state.db, &id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "task not found".into()));
    }

    let patch = TaskPatch {
        name: b.name,
        prompt_template: b.prompt_template,
        interval_seconds: b.interval_seconds,
        model: b.model,
        enabled: b.enabled,
    };
    let was_enabled = b.enabled;
    autonomous::update_task(&state.db, &id, patch)
        .await
        .map_err(internal)?;

    // Если задачу включили и у неё нет запланированного запуска — ставим
    // next_run_at=now чтобы scheduler сразу её подобрал.
    if let Some(true) = was_enabled {
        let t = autonomous::get_task(&state.db, &id)
            .await
            .map_err(internal)?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "task vanished".into()))?;
        if t.next_run_at.is_none() {
            let now = chrono::Utc::now().timestamp();
            autonomous::set_next_run(&state.db, &id, now)
                .await
                .map_err(internal)?;
        }
    }

    let t = autonomous::get_task(&state.db, &id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "task not found".into()))?;
    Ok(Json(serde_json::to_value(t).unwrap()))
}

// ────────── Delete ──────────

async fn delete_task(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let n = autonomous::delete_task(&state.db, &id)
        .await
        .map_err(internal)?;
    if n == 0 {
        // Идемпотентно: 204 даже при отсутствии — стандарт REST.
        tracing::debug!(id, "delete autonomous: no such task (idempotent 204)");
    }
    Ok(StatusCode::NO_CONTENT)
}

// ────────── Run-now ──────────

async fn run_now(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task = autonomous::get_task(&state.db, &id)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "task not found".into()))?;

    // HostApi для prompt-builder'а должен быть зарегистрирован
    // (set'ится в register_routes хост-приложения).
    let host = state
        .host
        .get()
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, "host adapter not set".into()))?;

    let task_id = task.id.clone();
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = runner::run_task(state_clone, host, task).await {
            tracing::warn!(task_id, error = %e, "run-now: run_task failed");
        }
    });

    Ok(Json(serde_json::json!({
        "ok": true,
        "task_id": id,
        "spawned": true,
    })))
}

// ────────── List runs ──────────

#[derive(Debug, Deserialize)]
struct RunsQuery {
    #[serde(default = "default_runs_limit")]
    limit: i64,
}
fn default_runs_limit() -> i64 {
    50
}

async fn list_runs(
    State(state): State<Arc<EchoState>>,
    Path(id): Path<String>,
    Query(q): Query<RunsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 404 если задачи нет.
    if autonomous::get_task(&state.db, &id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "task not found".into()));
    }
    let items = autonomous::list_runs(&state.db, &id, q.limit.max(1))
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ────────── Error type ──────────

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo autonomous route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use async_trait::async_trait;
    use echo_host_api::{HostApi, ProjectInfo, SessionInfo};
    use std::path::PathBuf;

    struct StubHost;
    #[async_trait]
    impl HostApi for StubHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(Vec::new())
        }
        async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn list_projects(&self) -> anyhow::Result<Vec<ProjectInfo>> {
            Ok(Vec::new())
        }
        async fn active_project_id(&self) -> Option<String> {
            None
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
    }

    async fn make_state() -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(PathBuf::from("/nope"), 1));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host).ok();
        state
    }

    #[tokio::test]
    async fn create_returns_created_with_next_run_at_set() {
        let state = make_state().await;
        let (status, body) = create_task(
            State(state),
            Json(serde_json::from_value(serde_json::json!({
                "name": "test",
                "prompt_template": "hi",
                "interval_seconds": 30,
                "model": "sonnet-4",
            }))
            .unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "test");
        assert!(body["next_run_at"].is_i64());
    }

    #[tokio::test]
    async fn create_rejects_zero_interval() {
        let state = make_state().await;
        let err = create_task(
            State(state),
            Json(serde_json::from_value(serde_json::json!({
                "name": "x",
                "prompt_template": "p",
                "interval_seconds": 0,
                "model": "m",
            }))
            .unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_returns_items() {
        let state = make_state().await;
        let _ = autonomous::create_task(&state.db, "a", "p", 60, "m", None).await.unwrap();
        let _ = autonomous::create_task(&state.db, "b", "p", 60, "m", None).await.unwrap();
        let resp = list_tasks(State(state)).await.unwrap();
        let items = resp.0["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn patch_disables_task() {
        let state = make_state().await;
        let t = autonomous::create_task(&state.db, "a", "p", 60, "m", None)
            .await
            .unwrap();
        let resp = patch_task(
            State(state.clone()),
            Path(t.id.clone()),
            Json(serde_json::from_value(serde_json::json!({"enabled": false})).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(resp.0["enabled"], false);
    }

    #[tokio::test]
    async fn patch_enable_with_null_next_run_sets_now() {
        let state = make_state().await;
        let t = autonomous::create_task(&state.db, "a", "p", 60, "m", None)
            .await
            .unwrap();
        // Сначала disable + симулируем NULL next_run_at через прямой UPDATE.
        let id = t.id.clone();
        state
            .db
            .conn()
            .call(move |c| {
                c.execute(
                    "UPDATE autonomous_tasks SET enabled = 0, next_run_at = NULL WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let _ = patch_task(
            State(state.clone()),
            Path(t.id.clone()),
            Json(serde_json::from_value(serde_json::json!({"enabled": true})).unwrap()),
        )
        .await
        .unwrap();
        let t2 = autonomous::get_task(&state.db, &t.id).await.unwrap().unwrap();
        assert!(t2.enabled);
        assert!(t2.next_run_at.is_some(), "next_run_at must be set when enabling");
    }

    #[tokio::test]
    async fn patch_rejects_unknown_id() {
        let state = make_state().await;
        let err = patch_task(
            State(state),
            Path("nope".into()),
            Json(serde_json::from_value(serde_json::json!({"enabled": false})).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_returns_204_idempotent() {
        let state = make_state().await;
        let st = delete_task(State(state.clone()), Path("missing".into()))
            .await
            .unwrap();
        assert_eq!(st, StatusCode::NO_CONTENT);
        let t = autonomous::create_task(&state.db, "a", "p", 60, "m", None).await.unwrap();
        let st = delete_task(State(state.clone()), Path(t.id)).await.unwrap();
        assert_eq!(st, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn list_runs_returns_404_for_unknown_task() {
        let state = make_state().await;
        let err = list_runs(
            State(state),
            Path("nope".into()),
            Query(RunsQuery { limit: 10 }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_runs_returns_items() {
        let state = make_state().await;
        let t = autonomous::create_task(&state.db, "a", "p", 60, "m", None).await.unwrap();
        autonomous::insert_run(&state.db, &t.id, 100).await.unwrap();
        autonomous::insert_run(&state.db, &t.id, 200).await.unwrap();
        let resp = list_runs(
            State(state),
            Path(t.id),
            Query(RunsQuery { limit: 10 }),
        )
        .await
        .unwrap();
        let items = resp.0["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn run_now_returns_ok_and_spawns() {
        // Без реального CLI runner-task выполнится с ошибкой,
        // но handler должен вернуть 200, потому что spawn успешен.
        let state = make_state().await;
        let t = autonomous::create_task(&state.db, "a", "p", 60, "m", None).await.unwrap();
        let resp = run_now(State(state.clone()), Path(t.id.clone())).await.unwrap();
        assert_eq!(resp.0["ok"], true);
        // Дать spawned task время добавить TaskRun.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let runs = autonomous::list_runs(&state.db, &t.id, 10).await.unwrap();
        assert!(!runs.is_empty(), "run-now should have inserted at least one run");
    }

    #[tokio::test]
    async fn run_now_returns_404_for_unknown_task() {
        let state = make_state().await;
        let err = run_now(State(state), Path("nope".into())).await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
