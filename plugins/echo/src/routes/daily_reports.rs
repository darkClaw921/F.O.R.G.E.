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
//!   - 200 — полный отчёт `DailyReport` (включая `suggestions`), тот же
//!     формат, что и у `GET /daily-reports/:day`.
//!   - 400 на невалидный формат `day`.
//!   - 504 при превышении таймаута.
//! - `GET  /api/echo/daily-reports/prompts` → текущие эффективные промпты
//!   `{ report_prompt, suggest_prompt, report_prompt_default, suggest_prompt_default }`.
//! - `PUT  /api/echo/daily-reports/prompts` body
//!   `{ report_prompt?, suggest_prompt? }` → сохранить/сбросить оверрайды
//!   промптов (пустая строка → сброс к дефолту), вернуть актуальное состояние.
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
use crate::db::repo::{app_settings, daily_reports};
use crate::state::EchoState;

/// Лимит по умолчанию для `GET /daily-reports`, когда `?limit=` не задан.
const DEFAULT_LIMIT: i64 = 30;

/// Таймаут на синхронную генерацию. Сводка дня собирает чаты, tmux-панели и
/// git-активность и прогоняет их через Claude CLI ДВУМЯ вызовами (основной
/// отчёт + предложения задач), которые идут параллельно, но на медленной
/// модели всё равно могут занять заметное время — поэтому даём 240с запаса.
/// По превышении возвращаем 504; фронтенд может повторить или дождаться
/// авто-генерации scheduler'ом.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(240);

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route("/api/echo/daily-reports", get(list))
        .route("/api/echo/daily-reports/generate", post(generate))
        // Статический сегмент `prompts` регистрируем ДО динамического `/:day`,
        // иначе axum матчит `prompts` как значение `:day`.
        .route(
            "/api/echo/daily-reports/prompts",
            get(get_prompts).put(put_prompts),
        )
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
        // Возвращаем полный отчёт целиком (включая `suggestions`), а не
        // выборочные поля — тот же формат, что и `GET /daily-reports/:day`.
        Ok(Ok(report)) => Ok(Json(serde_json::to_value(report).unwrap_or_default())),
        Ok(Err(e)) => Err(internal(e)),
        Err(_) => Err(ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            format!("generate exceeded {}s timeout", GENERATE_TIMEOUT.as_secs()),
        )),
    }
}

/// Текущий эффективный промпт: пользовательский оверрайд из `app_settings`
/// (непустой после trim), иначе дефолт-константа. Та же логика, что и в
/// [`crate::daily_report`] при генерации.
async fn effective_prompt(
    state: &Arc<EchoState>,
    key: &str,
    default: &str,
) -> Result<String, ApiError> {
    let value = app_settings::get(&state.db, key)
        .await
        .map_err(internal)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string());
    Ok(value)
}

/// Собирает текущее состояние промптов в JSON-ответ
/// `{ report_prompt, suggest_prompt, report_prompt_default, suggest_prompt_default }`.
async fn prompts_payload(state: &Arc<EchoState>) -> Result<serde_json::Value, ApiError> {
    let report_prompt = effective_prompt(
        state,
        daily_report::PROMPT_KEY_REPORT,
        daily_report::REPORT_META_PROMPT,
    )
    .await?;
    let suggest_prompt = effective_prompt(
        state,
        daily_report::PROMPT_KEY_SUGGEST,
        daily_report::SUGGEST_META_PROMPT,
    )
    .await?;
    Ok(serde_json::json!({
        "report_prompt": report_prompt,
        "suggest_prompt": suggest_prompt,
        "report_prompt_default": daily_report::REPORT_META_PROMPT,
        "suggest_prompt_default": daily_report::SUGGEST_META_PROMPT,
    }))
}

/// `GET /api/echo/daily-reports/prompts` → текущие эффективные промпты вместе
/// с дефолтами (для UI-кнопки «сбросить к дефолту»).
async fn get_prompts(
    State(state): State<Arc<EchoState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(prompts_payload(&state).await?))
}

#[derive(Debug, Deserialize)]
struct PutPromptsBody {
    #[serde(default)]
    report_prompt: Option<String>,
    #[serde(default)]
    suggest_prompt: Option<String>,
}

/// `PUT /api/echo/daily-reports/prompts` body
/// `{ report_prompt?: string, suggest_prompt?: string }`.
///
/// Для каждого ПРИСУТСТВУЮЩЕГО поля: trim; пустая строка → сброс оверрайда
/// ([`app_settings::delete`]), иначе сохранение ([`app_settings::set`]).
/// Отсутствующее поле не трогается. Возвращает актуальное состояние тем же
/// JSON, что и [`get_prompts`].
async fn put_prompts(
    State(state): State<Arc<EchoState>>,
    Json(b): Json<PutPromptsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    for (value, key) in [
        (b.report_prompt, daily_report::PROMPT_KEY_REPORT),
        (b.suggest_prompt, daily_report::PROMPT_KEY_SUGGEST),
    ] {
        if let Some(raw) = value {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                app_settings::delete(&state.db, key)
                    .await
                    .map_err(internal)?;
            } else {
                app_settings::set(&state.db, key, trimmed)
                    .await
                    .map_err(internal)?;
            }
        }
    }
    Ok(Json(prompts_payload(&state).await?))
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
    async fn generate_returns_suggestions_array() {
        let state = make_app().await;
        let app = build_router(state);
        // StubHost::collect_project_activity → default (пустой вектор), второй
        // one_shot не зовётся → suggestions == пустой массив, но ключ присутствует.
        let body = serde_json::json!({ "day": "2026-05-14" }).to_string();
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
        let suggestions = v["suggestions"]
            .as_array()
            .expect("response must contain `suggestions` array");
        assert!(suggestions.is_empty());
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

    async fn get_prompts_json(state: Arc<EchoState>) -> serde_json::Value {
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/echo/daily-reports/prompts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn put_prompts_json(state: Arc<EchoState>, body: serde_json::Value) -> serde_json::Value {
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/echo/daily-reports/prompts")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn get_prompts_clean_db_returns_defaults() {
        let state = make_app().await;
        let v = get_prompts_json(state).await;
        assert_eq!(v["report_prompt"], v["report_prompt_default"]);
        assert_eq!(v["suggest_prompt"], v["suggest_prompt_default"]);
        assert_eq!(v["report_prompt_default"], daily_report::REPORT_META_PROMPT);
        assert_eq!(
            v["suggest_prompt_default"],
            daily_report::SUGGEST_META_PROMPT
        );
    }

    #[tokio::test]
    async fn put_set_then_get_returns_new_value() {
        let state = make_app().await;
        let custom = "Мой собственный промпт отчёта";
        let put = put_prompts_json(state.clone(), serde_json::json!({ "report_prompt": custom }))
            .await;
        // PUT возвращает актуальное состояние.
        assert_eq!(put["report_prompt"], custom);
        // suggest не трогали → остаётся дефолтным.
        assert_eq!(put["suggest_prompt"], put["suggest_prompt_default"]);

        // Независимый GET видит сохранённое значение.
        let v = get_prompts_json(state).await;
        assert_eq!(v["report_prompt"], custom);
        // Дефолт-константа неизменна.
        assert_eq!(v["report_prompt_default"], daily_report::REPORT_META_PROMPT);
    }

    #[tokio::test]
    async fn put_empty_string_resets_to_default() {
        let state = make_app().await;
        // Сначала задаём оверрайд.
        put_prompts_json(
            state.clone(),
            serde_json::json!({ "suggest_prompt": "временный" }),
        )
        .await;
        let after_set = get_prompts_json(state.clone()).await;
        assert_eq!(after_set["suggest_prompt"], "временный");

        // Пустая строка → сброс к дефолту.
        let reset =
            put_prompts_json(state.clone(), serde_json::json!({ "suggest_prompt": "" })).await;
        assert_eq!(reset["suggest_prompt"], reset["suggest_prompt_default"]);

        let v = get_prompts_json(state).await;
        assert_eq!(v["suggest_prompt"], daily_report::SUGGEST_META_PROMPT);
    }

    #[tokio::test]
    async fn prompts_route_not_shadowed_by_day() {
        // Статический `prompts` не должен матчиться как `:day` (что дало бы 400
        // на невалидный формат даты).
        let state = make_app().await;
        let v = get_prompts_json(state).await;
        assert!(v["report_prompt"].is_string());
    }
}
