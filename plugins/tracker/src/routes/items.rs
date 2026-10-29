//! Роуты пунктов чек-листа и их персональных таймеров.
//!
//! # Пункт адресуется парой `(task_id, item_id)`
//!
//! Стор умеет находить пункт по одному лишь `item_id`
//! ([`crate::store::TrackerStore::update_item`] и соседи), но роуты живут под
//! `/api/tracker/tasks/:id/items/:item_id` и обязаны ПРОВЕРЯТЬ принадлежность.
//! Без проверки `PATCH /tasks/A/items/<пункт задачи B>` молча правил бы чужой
//! пункт, а UI, отрисовавший ответ в карточке задачи A, показал бы чужой текст.
//! Проверку делает [`require_item`]; она же даёт требуемые 404 на пункт
//! несуществующей задачи.
//!
//! # Время пункта — это время задачи
//!
//! Таймер пункта заводит обычный [`crate::models::TimeEntry`] с заполненным
//! `item_id` и тем же `task_id`. Отдельного счётчика у пункта нет и быть не
//! должно: сумма по задаче считается по её отрезкам, поэтому время пункта
//! попадает в неё РОВНО ОДИН РАЗ — не как слагаемое сверху, а как часть той же
//! записи. Инвариант «один активный отрезок на задачу» действует и здесь:
//! запустить таймер на пункте, пока идёт таймер самой задачи, значит получить
//! обратно уже идущий отрезок (стор не создаёт второй).
//!
//! # События
//!
//! Каждая мутация пункта публикует `task_upsert` РОДИТЕЛЬСКОЙ задачи: от
//! чек-листа зависит её прогресс `n/m`, и панель обязана перерисовать карточку.
//! Мутации, которые вдобавок трогают отрезки (таймер пункта), публикуют ещё и
//! `entry_upsert`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::{ApiError, ApiResult};
use super::parse_body;
use crate::models::{ChecklistItem, ItemPatch, TimeEntry};
use crate::state::TrackerState;
use crate::ws::TrackerEvent;

/// Тело `POST /api/tracker/tasks/:id/items`.
#[derive(Debug, Deserialize)]
pub struct CreateItemReq {
    /// Текст пункта.
    pub title: String,
}

/// Тело `POST /api/tracker/tasks/:id/items/reorder`.
#[derive(Debug, Deserialize)]
pub struct ReorderReq {
    /// Идентификаторы пунктов в новом порядке.
    pub ids: Vec<String>,
}

/// Тело таймера пункта: необязательный комментарий к отрезку.
#[derive(Debug, Default, Deserialize)]
pub struct TimerReq {
    /// Комментарий, который попадёт в `TimeEntry.note`.
    #[serde(default)]
    pub note: Option<String>,
}

/// Проверяет, что задача существует. 404, если нет.
pub fn require_task(state: &TrackerState, task_id: &str) -> ApiResult<()> {
    if state.store.task(task_id).is_none() {
        return Err(ApiError::not_found(format!("task {task_id}")));
    }
    Ok(())
}

/// Проверяет, что задача существует и пункт принадлежит именно ей.
fn require_item(state: &TrackerState, task_id: &str, item_id: &str) -> ApiResult<ChecklistItem> {
    require_task(state, task_id)?;
    state
        .store
        .items(task_id)
        .into_iter()
        .find(|i| i.id == item_id)
        .ok_or_else(|| ApiError::not_found(format!("checklist item {item_id} of task {task_id}")))
}

/// Публикует кадр «задача изменилась» — прогресс чек-листа поехал.
fn emit_task(state: &TrackerState, task_id: &str) {
    if let Some(task) = state.store.task(task_id) {
        state.emit(TrackerEvent::task_upsert(task));
    }
}

/// `GET /api/tracker/tasks/:id/items` — пункты в порядке `order`.
pub async fn list_items(
    State(state): State<Arc<TrackerState>>,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Vec<ChecklistItem>>> {
    require_task(&state, &task_id)?;
    Ok(Json(state.store.items(&task_id)))
}

/// `POST /api/tracker/tasks/:id/items` — добавить пункт.
pub async fn create_item(
    State(state): State<Arc<TrackerState>>,
    Path(task_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<ChecklistItem>)> {
    let req: CreateItemReq = parse_body(body)?;
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    let item = state.store.add_item(&task_id, &title)?;
    emit_task(&state, &task_id);
    Ok((axum::http::StatusCode::CREATED, Json(item)))
}

/// `PATCH /api/tracker/tasks/:id/items/:item_id` — текст, галочка, порядок.
pub async fn patch_item(
    State(state): State<Arc<TrackerState>>,
    Path((task_id, item_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<ChecklistItem>> {
    require_item(&state, &task_id, &item_id)?;
    let patch: ItemPatch = parse_body(body)?;
    let item = state.store.update_item(&item_id, patch)?;
    emit_task(&state, &task_id);
    Ok(Json(item))
}

/// `POST /api/tracker/tasks/:id/items/:item_id/toggle` — переключить галочку.
///
/// Отдельный роут рядом с общим патчем: клик по чекбоксу не должен требовать
/// от фронта знания текущего состояния пункта (иначе два быстрых клика по
/// устаревшему состоянию дали бы «отметить» дважды вместо «отметить/снять»).
pub async fn toggle_item(
    State(state): State<Arc<TrackerState>>,
    Path((task_id, item_id)): Path<(String, String)>,
) -> ApiResult<Json<ChecklistItem>> {
    require_item(&state, &task_id, &item_id)?;
    let item = state.store.toggle_item(&item_id)?;
    emit_task(&state, &task_id);
    Ok(Json(item))
}

/// `POST /api/tracker/tasks/:id/items/reorder` — новый порядок пунктов.
pub async fn reorder_items(
    State(state): State<Arc<TrackerState>>,
    Path(task_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Vec<ChecklistItem>>> {
    require_task(&state, &task_id)?;
    let req: ReorderReq = parse_body(body)?;
    state.store.reorder_items(&task_id, &req.ids)?;
    emit_task(&state, &task_id);
    Ok(Json(state.store.items(&task_id)))
}

/// `DELETE /api/tracker/tasks/:id/items/:item_id` — удалить пункт.
///
/// Отрезки, привязанные к пункту, НЕ удаляются: время потрачено, и оно
/// остаётся в сумме задачи. У осиротевших отрезков `item_id` продолжает
/// указывать на исчезнувший пункт — сумма задачи считается по `task_id`, а
/// отчёт по пунктам строится от списка существующих пунктов, поэтому ссылка
/// в никуда ничего не искажает.
pub async fn delete_item(
    State(state): State<Arc<TrackerState>>,
    Path((task_id, item_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    require_item(&state, &task_id, &item_id)?;
    state.store.delete_item(&item_id)?;
    emit_task(&state, &task_id);
    Ok(Json(serde_json::json!({ "ok": true, "id": item_id })))
}

/// `POST /api/tracker/tasks/:id/items/:item_id/timer/start` — таймер пункта.
///
/// Возвращает идущий отрезок задачи, если он уже есть (в том числе заведённый
/// без привязки к пункту) — см. инвариант в шапке модуля.
pub async fn start_item_timer(
    State(state): State<Arc<TrackerState>>,
    Path((task_id, item_id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<TimeEntry>> {
    require_item(&state, &task_id, &item_id)?;
    let req: TimerReq = match body {
        Some(Json(v)) => parse_body(v)?,
        None => TimerReq::default(),
    };
    let entry = state
        .store
        .start_timer(&task_id, Some(item_id.as_str()), req.note)?;
    state.emit(TrackerEvent::entry_upsert(entry.clone()));
    emit_task(&state, &task_id);
    Ok(Json(entry))
}

/// `POST /api/tracker/tasks/:id/items/:item_id/timer/stop` — остановить.
///
/// Останавливает активный отрезок ЗАДАЧИ (он у неё один), даже если тот был
/// начат без привязки к пункту: иначе кнопка «стоп» на пункте молча не делала
/// бы ничего.
pub async fn stop_item_timer(
    State(state): State<Arc<TrackerState>>,
    Path((task_id, item_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    require_item(&state, &task_id, &item_id)?;
    let stopped = state.store.stop_timer(&task_id)?;
    if let Some(entry) = stopped.clone() {
        state.emit(TrackerEvent::entry_upsert(entry));
        emit_task(&state, &task_id);
    }
    Ok(Json(serde_json::json!({
        "stopped": stopped.is_some(),
        "entry": stopped,
    })))
}
