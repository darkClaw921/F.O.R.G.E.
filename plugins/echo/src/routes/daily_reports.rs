//! REST API для «Сводки дня» (`daily_reports`).
//!
//! - `GET  /api/echo/daily-reports?limit=N` → список последних `N` отчётов
//!   (по умолчанию [`DEFAULT_LIMIT`]), отсортированных по `day DESC`.
//! - `GET  /api/echo/daily-reports/:day` → отчёт за конкретный локальный день
//!   (`YYYY-MM-DD`); 404 если за этот день записи нет.
//! - `POST /api/echo/daily-reports/generate` body `{ day?: "YYYY-MM-DD" }`
//!   → синхронная генерация через [`crate::daily_report::generate_report`]
//!   (`source = "manual"`) с таймаутом [`GENERATE_TIMEOUT`].
//!   - `day` по умолчанию — сегодня по локальному времени.
//!   - 200 `{ id, day, content }` при успехе.
//!   - 400 на невалидный формат `day`.
//!   - 504 при превышении таймаута.
//!
//! Ошибки сериализуются как `{ "error": "..." }` со статусом 400/404/500/504
//! (та же конвенция [`ApiError`], что и в [`crate::routes::memory`]).

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::daily_report;
use crate::db::repo::daily_reports;
use crate::state::EchoState;

/// Лимит по умолчанию для `GET /daily-reports`, когда `?limit=` не задан.
const DEFAULT_LIMIT: i64 = 30;

/// Таймаут на синхронную генерацию. Сводка дня собирает чаты, tmux-панели и
/// git-активность и прогоняет их через Claude CLI — это может занять больше
/// времени, чем обычный memory-rollover, поэтому даём 90с запаса. По
/// превышении возвращаем 504; фронтенд может повторить или дождаться
/// авто-генерации scheduler'ом.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(90);

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route("/api/echo/daily-reports", get(list))
        .route("/api/echo/daily-reports/generate", post(generate))
        .route("/api/echo/daily-reports/:day", get(get_by_day))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

/// `GET /api/echo/daily-reports?limit=N` → `{ items: [DailyReport, ...] }`.
async fn list(
    State(state): State<Arc<EchoState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 365);
    let items = daily_reports::list(&state.db, limit)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

/// `GET /api/echo/daily-reports/:day` → отчёт за день или 404.
async fn get_by_day(
    State(state): State<Arc<EchoState>>,
    Path(day): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Валидируем формат, чтобы не считать произвольную строку валидным днём.
    chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid day: {e}")))?;

    let report = daily_reports::get_by_day(&state.db, &day)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no report for day {day}")))?;

    Ok(Json(serde_json::to_value(report).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
struct GenerateBody {
    #[serde(default)]
    day: Option<String>,
}

/// `POST /api/echo/daily-reports/generate` — синхронная генерация сводки.
async fn generate(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<GenerateBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let day = match b.day.as_deref() {
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid day: {e}")))?,
        None => chrono::Local::now().date_naive(),
    };

    let host = state.host.get().cloned().ok_or_else(|| {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "host adapter missing".into())
    })?;

    let fut = daily_report::generate_report(state.clone(), host, day, "manual");

    match tokio::time::timeout(GENERATE_TIMEOUT, fut).await {
        Ok(Ok(report)) => Ok(Json(serde_json::json!({
            "id": report.id,
            "day": report.day,
            "content": report.content,
        }))),
        Ok(Err(e)) => Err(internal(e)),
        Err(_) => Err(ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            format!("generate exceeded {}s timeout", GENERATE_TIMEOUT.as_secs()),
        )),
    }
}

/// Простая JSON-ошибка с произвольным статусом (та же конвенция, что и
/// в [`crate::routes::memory`]).
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo daily_reports route error: {e:#}");
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
        // collect_git_activity использует default → Ok("").
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
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"## Что сделано\nработа"}}'
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
    async fn generate_invalid_day_returns_400() {
        let state = make_app().await;
        let app = build_router(state);
        let body = serde_json::json!({ "day": "not-a-date" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/daily-reports/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn generate_explicit_day_success() {
        let state = make_app().await;
        let app = build_router(state.clone());
        // Empty day → NO_ACTIVITY_RU (no messages/panes/git), runner не нужен.
        let body = serde_json::json!({ "day": "2026-05-16" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/daily-reports/generate")
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
        assert_eq!(v["day"], "2026-05-16");
        assert!(v["id"].as_str().unwrap().len() > 0);
        assert!(v["content"].as_str().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn generate_default_day_today() {
        let state = make_app().await;
        let app = build_router(state);
        // Без day → сегодня local; пустой день → NO_ACTIVITY_RU, 200.
        let body = serde_json::json!({}).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/echo/daily-reports/generate")
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
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        assert_eq!(v["day"], today);
    }

    #[tokio::test]
    async fn get_by_day_404_when_missing() {
        let state = make_app().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/echo/daily-reports/2099-01-01")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_by_day_400_on_bad_format() {
        let state = make_app().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/echo/daily-reports/garbage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_and_get_roundtrip() {
        let state = make_app().await;
        // Сгенерим запись через generate (пустой день → NO_ACTIVITY_RU).
        let app = build_router(state.clone());
        let body = serde_json::json!({ "day": "2026-05-15" }).to_string();
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/echo/daily-reports/generate")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

        // GET list → содержит запись.
        let app = build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/echo/daily-reports")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["day"], "2026-05-15");

        // GET :day → та же запись (200).
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/echo/daily-reports/2026-05-15")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["day"], "2026-05-15");
    }
}
