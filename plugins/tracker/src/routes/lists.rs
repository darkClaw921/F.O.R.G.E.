//! Роуты списков задач (`// LISTS` в левой колонке панели).
//!
//! # Удаление списка НЕ удаляет задачи
//!
//! Принятое правило: `DELETE /api/tracker/lists/:id` убирает только сам
//! список, а у задач и вех обнуляет `list_id` — задачи переезжают в `~/inbox`,
//! а не исчезают. Иначе одно нажатие на крестик у папки уносило бы вместе с
//! ней историю времени по всем её задачам, причём безвозвратно: журнала
//! удалений в трекере нет.
//!
//! Каскад выполняет [`crate::store::TrackerStore::delete_list`], роут его
//! только вызывает.
//!
//! # События
//!
//! Все мутации списков публикуют `reload`: точечного кадра для списка в
//! [`TrackerEvent`] нет намеренно (см. шапку [`crate::ws`]), а изменение
//! списка почти всегда затрагивает и задачи (обнуление `list_id`), то есть
//! точечный кадр всё равно не описал бы последствие целиком.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::{ApiError, ApiResult};
use super::parse_body;
use crate::models::{ListPatch, TaskList};
use crate::state::TrackerState;
use crate::ws::TrackerEvent;

/// Тело `POST /api/tracker/lists`.
#[derive(Debug, Deserialize)]
pub struct CreateListReq {
    /// Имя списка.
    pub name: String,
    /// Цвет метки в панели.
    #[serde(default)]
    pub color: Option<String>,
}

/// Тело `POST /api/tracker/lists/reorder`.
#[derive(Debug, Deserialize)]
pub struct ReorderReq {
    /// Идентификаторы в новом порядке.
    pub ids: Vec<String>,
}

/// `GET /api/tracker/lists` — списки в порядке отображения.
pub async fn list_lists(State(state): State<Arc<TrackerState>>) -> Json<Vec<TaskList>> {
    Json(state.store.lists())
}

/// `POST /api/tracker/lists` — создать список.
pub async fn create_list(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<TaskList>)> {
    let req: CreateListReq = parse_body(body)?;
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    let list = state.store.create_list(&name, req.color)?;
    state.emit(TrackerEvent::Reload);
    Ok((axum::http::StatusCode::CREATED, Json(list)))
}

/// `PATCH /api/tracker/lists/:id` — переименовать, перекрасить, переставить.
pub async fn patch_list(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<TaskList>> {
    let patch: ListPatch = parse_body(body)?;
    let list = state.store.update_list(&id, patch)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(list))
}

/// `POST /api/tracker/lists/reorder` — новый порядок списков.
pub async fn reorder_lists(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Vec<TaskList>>> {
    let req: ReorderReq = parse_body(body)?;
    state.store.reorder_lists(&req.ids)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(state.store.lists()))
}

/// `DELETE /api/tracker/lists/:id` — удалить список, сохранив его задачи.
pub async fn delete_list(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.delete_list(&id)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}
