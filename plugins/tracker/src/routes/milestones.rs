//! Роуты вех — дорожка `// MILESTONES` (`◆`) на диаграмме Гантта.
//!
//! Веха это точка, а не отрезок: у неё есть дата и подпись, но нет ни
//! длительности, ни времени, ни статуса. Поэтому она НЕ задача и живёт
//! отдельной коллекцией: попытка выразить её задачей со статусом `done`
//! засорила бы канбан и отчёты записями, по которым никто никогда не работал.
//!
//! # Дата проверяется на входе
//!
//! Поле `date` принимает и `YYYY-MM-DD`, и полный RFC 3339 (голая дата
//! трактуется как локальная полночь — [`crate::report::parse_ts`]). Значение,
//! которое не разбирается, отбивается на роуте кодом 400: сохранить его молча
//! означало бы получить веху, невидимую на Гантте, без единого признака
//! ошибки — она просто не попала бы ни в одно окно.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::{ApiError, ApiResult};
use super::parse_body;
use crate::models::{Milestone, MilestonePatch};
use crate::state::TrackerState;
use crate::ws::TrackerEvent;

/// Тело `POST /api/tracker/milestones`.
#[derive(Debug, Deserialize)]
pub struct CreateMilestoneReq {
    /// Подпись вехи.
    pub title: String,
    /// Дата: `YYYY-MM-DD` или полный RFC 3339.
    pub date: String,
    /// Список, к дорожке которого относится веха (`null` — общая).
    #[serde(default)]
    pub list_id: Option<String>,
    /// Цвет маркера.
    #[serde(default)]
    pub color: Option<String>,
}

/// Проверяет, что дата разбирается. 400, если нет.
fn require_date(raw: &str) -> ApiResult<()> {
    if crate::report::parse_ts(raw).is_none() {
        return Err(ApiError::bad_request(format!(
            "date must be YYYY-MM-DD or RFC 3339, got {raw:?}"
        )));
    }
    Ok(())
}

/// `GET /api/tracker/milestones` — вехи по возрастанию даты.
pub async fn list_milestones(State(state): State<Arc<TrackerState>>) -> Json<Vec<Milestone>> {
    Json(state.store.milestones())
}

/// `POST /api/tracker/milestones` — создать веху.
pub async fn create_milestone(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<Milestone>)> {
    let req: CreateMilestoneReq = parse_body(body)?;
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    require_date(&req.date)?;
    let ms = state
        .store
        .create_milestone(&title, req.date.trim(), req.list_id, req.color)?;
    state.emit(TrackerEvent::Reload);
    Ok((axum::http::StatusCode::CREATED, Json(ms)))
}

/// `PATCH /api/tracker/milestones/:id` — подпись, дата, список, цвет.
pub async fn patch_milestone(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Milestone>> {
    let patch: MilestonePatch = parse_body(body)?;
    if let Some(date) = patch.date.as_deref() {
        require_date(date)?;
    }
    let ms = state.store.update_milestone(&id, patch)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(ms))
}

/// `DELETE /api/tracker/milestones/:id` — удалить веху.
pub async fn delete_milestone(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.delete_milestone(&id)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}
