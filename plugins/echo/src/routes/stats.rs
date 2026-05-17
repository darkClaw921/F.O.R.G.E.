//! REST API статистики токенов + публичный cancel-эндпоинт.
//!
//! - `GET  /api/echo/stats?range=hour|day` — bucket'ы из `token_stats`.
//!   - `range=hour` → последние 60 минут (60 minute-bucket'ов).
//!   - `range=day`  → последние 24 часа агрегированные по часу (24 bucket'а).
//! - `POST /api/echo/run/:id/cancel` — отмена активного run'а через
//!   `ClaudeRunner::cancel`. Обычно cancel идёт через WebSocket
//!   (`ClientMsg::Cancel`), но REST-вариант нужен:
//!   - на случай, если WS-соединение умерло,
//!   - для CLI-инструментов / curl-смоук.
//!
//! Response shape для `/stats`:
//!
//! ```json
//! {
//!   "range": "hour",
//!   "buckets": [
//!     {"ts": 1779580800, "tokens_in": 10, "tokens_out": 5,
//!      "cache_creation": 0, "cache_read": 0},
//!     ...
//!   ]
//! }
//! ```

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::db::repo::stats;
use crate::state::EchoState;

pub fn router() -> Router<Arc<EchoState>> {
    Router::new()
        .route("/api/echo/stats", get(get_stats))
        .route("/api/echo/run/:run_id/cancel", post(cancel_run))
}

#[derive(Debug, Deserialize)]
struct StatsQuery {
    /// `hour` (60 минутных bucket'ов) или `day` (24 часовых bucket'а).
    #[serde(default = "default_range")]
    range: String,
}
fn default_range() -> String {
    "hour".to_string()
}

/// JSON-shape одного bucket'а.
#[derive(Debug, Serialize)]
struct BucketDto {
    ts: i64,
    tokens_in: i64,
    tokens_out: i64,
    cache_creation: i64,
    cache_read: i64,
}

async fn get_stats(
    State(state): State<Arc<EchoState>>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    let now_bucket = now / 60;

    let buckets: Vec<BucketDto> = match q.range.as_str() {
        "hour" => {
            // Окно: 60 последних bucket'ов (минут).
            let from = now_bucket - 59;
            let raw = stats::range(&state.db, from, now_bucket)
                .await
                .map_err(internal)?;
            // Карта существующих, потом fill нулями для пустых минут.
            use std::collections::HashMap;
            let map: HashMap<i64, _> =
                raw.into_iter().map(|b| (b.bucket_minute, b)).collect();
            (from..=now_bucket)
                .map(|bm| {
                    let ts = bm * 60;
                    if let Some(b) = map.get(&bm) {
                        BucketDto {
                            ts,
                            tokens_in: b.tokens_in,
                            tokens_out: b.tokens_out,
                            cache_creation: b.cache_creation,
                            cache_read: b.cache_read,
                        }
                    } else {
                        BucketDto {
                            ts,
                            tokens_in: 0,
                            tokens_out: 0,
                            cache_creation: 0,
                            cache_read: 0,
                        }
                    }
                })
                .collect()
        }
        "day" => {
            // 24 часовых bucket'а. Каждый = сумма 60 минутных.
            let mut out = Vec::with_capacity(24);
            for hour_back in (0..24).rev() {
                let hour_start_bucket = now_bucket - hour_back * 60 - 59;
                let hour_end_bucket = now_bucket - hour_back * 60;
                let raw = stats::range(&state.db, hour_start_bucket, hour_end_bucket)
                    .await
                    .map_err(internal)?;
                let mut tin = 0i64;
                let mut tout = 0i64;
                let mut cc = 0i64;
                let mut cr = 0i64;
                for b in raw {
                    tin += b.tokens_in;
                    tout += b.tokens_out;
                    cc += b.cache_creation;
                    cr += b.cache_read;
                }
                out.push(BucketDto {
                    ts: hour_end_bucket * 60,
                    tokens_in: tin,
                    tokens_out: tout,
                    cache_creation: cc,
                    cache_read: cr,
                });
            }
            out
        }
        other => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!("unsupported range '{other}', expected 'hour' or 'day'"),
            ));
        }
    };

    Ok(Json(serde_json::json!({
        "range": q.range,
        "buckets": buckets,
    })))
}

async fn cancel_run(
    State(state): State<Arc<EchoState>>,
    Path(run_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.runner.cancel(&run_id).await {
        Ok(StatusCode::OK)
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("run {run_id} not found or already finished"),
        ))
    }
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!("forge-echo stats route error: {e:#}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use std::path::PathBuf;

    async fn make_state() -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(PathBuf::from("/nope"), 1));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        Arc::new(EchoState::new(Arc::new(db), runner))
    }

    #[tokio::test]
    async fn stats_hour_returns_60_buckets() {
        let state = make_state().await;
        let resp = get_stats(
            State(state),
            Query(StatsQuery {
                range: "hour".into(),
            }),
        )
        .await
        .unwrap();
        let v = resp.0;
        let buckets = v["buckets"].as_array().expect("array");
        assert_eq!(buckets.len(), 60, "expected 60 minute buckets");
        assert_eq!(v["range"], "hour");
    }

    #[tokio::test]
    async fn stats_day_returns_24_buckets() {
        let state = make_state().await;
        let resp = get_stats(
            State(state),
            Query(StatsQuery {
                range: "day".into(),
            }),
        )
        .await
        .unwrap();
        let buckets = resp.0["buckets"].as_array().expect("array");
        assert_eq!(buckets.len(), 24);
    }

    #[tokio::test]
    async fn stats_bad_range_400() {
        let state = make_state().await;
        let err = get_stats(
            State(state),
            Query(StatsQuery {
                range: "year".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn stats_includes_inserted_bucket() {
        let state = make_state().await;
        let now = chrono::Utc::now().timestamp();
        stats::add_tokens(&state.db, now, 7, 3, 0, 0).await.unwrap();
        let resp = get_stats(
            State(state),
            Query(StatsQuery {
                range: "hour".into(),
            }),
        )
        .await
        .unwrap();
        let buckets = resp.0["buckets"].as_array().expect("array");
        let total_in: i64 = buckets
            .iter()
            .map(|b| b["tokens_in"].as_i64().unwrap_or(0))
            .sum();
        let total_out: i64 = buckets
            .iter()
            .map(|b| b["tokens_out"].as_i64().unwrap_or(0))
            .sum();
        assert_eq!(total_in, 7);
        assert_eq!(total_out, 3);
    }

    #[tokio::test]
    async fn cancel_run_returns_404_when_no_such_run() {
        let state = make_state().await;
        let err = cancel_run(State(state), Path("nonexistent-id".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
