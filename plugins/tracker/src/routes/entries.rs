//! Роуты таймеров задачи и ручной правки отрезков времени.
//!
//! # Параллельность и её единственное ограничение
//!
//! `POST /tasks/:id/timer/start` НИКОГДА не останавливает чужие таймеры.
//! Одновременно идущие отрезки по РАЗНЫМ задачам — штатный сценарий, ради
//! которого трекер и писался (созвон и правка кода идут вместе). Единственное
//! ограничение действует внутри одной задачи: повторный `start` по ней
//! возвращает УЖЕ ИДУЩИЙ отрезок с тем же `id`, а не заводит второй. Инвариант
//! держит [`crate::store::TrackerStore::start_timer`], роут его не дублирует —
//! иначе правило жило бы в двух местах и разъехалось бы при первой же правке.
//!
//! # Почему стоп отвечает 200, а не 409
//!
//! `POST /tasks/:id/timer/stop` по задаче без активного таймера возвращает
//! `{"stopped": false}` и код 200. Это не мягкость, а описание реальности:
//! кнопка «стоп» нажимается по состоянию, которое UI видел мгновение назад, и
//! гонка с автосохранением или со второй вкладкой — обычное дело. Ошибка
//! здесь заставила бы фронт показывать пользователю красный тост на действие,
//! которое привело ровно к желаемому результату.
//!
//! # 400 против 409 на отрезках
//!
//! Перевёрнутый интервал (`stopped_at <= started_at`) и неразбираемая отметка
//! времени — это **400**: такой запрос не станет законным ни при каком
//! состоянии стора. Проверка делается ЗДЕСЬ, до обращения к стору, причём для
//! `PATCH` — по ИТОГОВОМУ состоянию отрезка (патч мог сдвинуть одну границу,
//! оставив вторую), иначе правка «подвинь начало на час вперёд» могла бы
//! вывернуть отрезок наизнанку и дать отрицательную длительность в отчёте.
//!
//! Вторая попытка сделать отрезок активным при уже идущем таймере той же
//! задачи — **409** из стора: запрос валиден, его отвергает состояние.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::{ApiError, ApiResult};
use super::parse_body;
use crate::models::{EntryPatch, TimeEntry};
use crate::state::TrackerState;
use crate::ws::TrackerEvent;

/// Тело `POST /api/tracker/tasks/:id/timer/start`.
#[derive(Debug, Default, Deserialize)]
pub struct StartTimerReq {
    /// Комментарий к отрезку.
    #[serde(default)]
    pub note: Option<String>,
    /// Пункт чек-листа, по которому идёт работа.
    #[serde(default)]
    pub item_id: Option<String>,
}

/// Тело `POST /api/tracker/entries` — ручной отрезок задним числом.
#[derive(Debug, Deserialize)]
pub struct CreateEntryReq {
    /// Задача, к которой относится время.
    pub task_id: String,
    /// Начало (RFC 3339).
    pub started_at: String,
    /// Конец; `null` — восстановление потерянного активного таймера.
    #[serde(default)]
    pub stopped_at: Option<String>,
    /// Пункт чек-листа.
    #[serde(default)]
    pub item_id: Option<String>,
    /// Комментарий.
    #[serde(default)]
    pub note: Option<String>,
}

/// Проверяет пару отметок времени. 400 на неразбираемое и на перевёрнутое.
fn require_valid_span(started_at: &str, stopped_at: Option<&str>) -> ApiResult<()> {
    let start = crate::report::parse_ts(started_at)
        .ok_or_else(|| ApiError::bad_request(format!("bad started_at: {started_at:?}")))?;
    if let Some(raw) = stopped_at {
        let stop = crate::report::parse_ts(raw)
            .ok_or_else(|| ApiError::bad_request(format!("bad stopped_at: {raw:?}")))?;
        if stop <= start {
            return Err(ApiError::bad_request(
                "stopped_at must be later than started_at",
            ));
        }
    }
    Ok(())
}

/// `POST /api/tracker/tasks/:id/timer/start` — начать отсчёт.
pub async fn start_timer(
    State(state): State<Arc<TrackerState>>,
    Path(task_id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<TimeEntry>> {
    let req: StartTimerReq = match body {
        Some(Json(v)) => parse_body(v)?,
        None => StartTimerReq::default(),
    };
    let entry = state
        .store
        .start_timer(&task_id, req.item_id.as_deref(), req.note)?;
    state.emit(TrackerEvent::entry_upsert(entry.clone()));
    Ok(Json(entry))
}

/// `POST /api/tracker/tasks/:id/timer/stop` — остановить отсчёт.
pub async fn stop_timer(
    State(state): State<Arc<TrackerState>>,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let stopped = state.store.stop_timer(&task_id)?;
    if let Some(entry) = stopped.clone() {
        state.emit(TrackerEvent::entry_upsert(entry));
    }
    Ok(Json(serde_json::json!({
        "stopped": stopped.is_some(),
        "entry": stopped,
    })))
}

/// `GET /api/tracker/entries` — все отрезки, `?task_id=` сужает до одной задачи.
pub async fn list_entries(
    State(state): State<Arc<TrackerState>>,
    axum::extract::Query(query): axum::extract::Query<EntriesQuery>,
) -> ApiResult<Json<Vec<TimeEntry>>> {
    match query.task_id.as_deref() {
        Some(task_id) => {
            if state.store.task(task_id).is_none() {
                return Err(ApiError::not_found(format!("task {task_id}")));
            }
            Ok(Json(state.store.entries(task_id)))
        }
        None => Ok(Json(state.store.all_entries())),
    }
}

/// Query `GET /api/tracker/entries`.
#[derive(Debug, Default, Deserialize)]
pub struct EntriesQuery {
    /// Сузить выдачу до одной задачи.
    #[serde(default)]
    pub task_id: Option<String>,
}

/// `POST /api/tracker/entries` — завести отрезок вручную.
pub async fn create_entry(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<TimeEntry>)> {
    let req: CreateEntryReq = parse_body(body)?;
    require_valid_span(&req.started_at, req.stopped_at.as_deref())?;
    let entry = state.store.create_entry(
        &req.task_id,
        req.item_id.as_deref(),
        &req.started_at,
        req.stopped_at.as_deref(),
        req.note,
    )?;
    state.emit(TrackerEvent::entry_upsert(entry.clone()));
    Ok((axum::http::StatusCode::CREATED, Json(entry)))
}

/// `PATCH /api/tracker/entries/:id` — правка отрезка задним числом.
pub async fn patch_entry(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<TimeEntry>> {
    let patch: EntryPatch = parse_body(body)?;

    // Итоговое состояние собирается ДО обращения к стору: проверять надо не
    // патч, а то, что получится после его применения.
    let current = state
        .store
        .all_entries()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| ApiError::not_found(format!("entry {id}")))?;
    let started = patch
        .started_at
        .clone()
        .unwrap_or_else(|| current.started_at.clone());
    let stopped = match patch.stopped_at.clone() {
        Some(v) => v,
        None => current.stopped_at.clone(),
    };
    require_valid_span(&started, stopped.as_deref())?;

    let entry = state.store.update_entry(&id, patch)?;
    state.emit(TrackerEvent::entry_upsert(entry.clone()));
    Ok(Json(entry))
}

/// `DELETE /api/tracker/entries/:id` — удалить отрезок.
pub async fn delete_entry(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.delete_entry(&id)?;
    state.emit(TrackerEvent::entry_removed(id.clone()));
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}
