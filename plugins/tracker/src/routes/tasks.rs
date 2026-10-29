//! Роуты задач: CRUD, перестановка и смена колонки канбана.
//!
//! # Почему тела запросов разбираются в два шага
//!
//! Каждый мутирующий роут принимает `Json<Value>` и только потом переводит
//! значение в типизированную структуру через [`super::parse_body`]. Прямой
//! экстрактор `Json<CreateTaskReq>` был бы короче, но отвечал бы на кривое тело
//! кодом **422** мимо всего контракта ошибок: тело ответа сформировал бы axum,
//! а не [`ApiError`], и фронт получил бы форму, которой не знает. Два шага
//! дают ровно один формат ошибки на всю поверхность плагина.
//!
//! Отдельная причина есть у поля `status`: значения колонок канбана
//! (`inbox|today|doing|waiting|done`) приходят от фронта строкой, и неизвестное
//! значение обязано давать 400 с внятным перечислением допустимых. Через
//! автоматический разбор `TaskStatus` вышло бы 422 «unknown variant», что
//! требование задачи прямо запрещает.
//!
//! # События
//!
//! Точечные мутации публикуют `task_upsert` / `task_removed`. Перестановка
//! (`POST /tasks/reorder`) меняет `order` у ПРОИЗВОЛЬНОГО числа задач разом,
//! поэтому публикует `reload`: слать десяток `task_upsert` подряд означало бы
//! с гарантией переполнять буфер канала на каждой сортировке списка.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::error::{ApiError, ApiResult};
use super::{parse_body, parse_status};
use crate::models::{TaskPatch, TaskStatus, TrackerTask};
use crate::state::TrackerState;
use crate::store::NewTask;
use crate::ws::TrackerEvent;

/// Тело `POST /api/tracker/tasks`.
#[derive(Debug, Deserialize)]
pub struct CreateTaskReq {
    /// Заголовок; единственное обязательное поле.
    pub title: String,
    /// Список, к которому относится задача.
    #[serde(default)]
    pub list_id: Option<String>,
    /// Заметки.
    #[serde(default)]
    pub notes: Option<String>,
    /// Колонка канбана; по умолчанию `inbox`.
    #[serde(default)]
    pub status: Option<String>,
    /// Приоритет 0..4; по умолчанию 2.
    #[serde(default)]
    pub priority: Option<u8>,
    /// Теги.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Оценка в минутах.
    #[serde(default)]
    pub estimate_min: Option<u32>,
    /// Плановое начало для Гантта.
    #[serde(default)]
    pub start: Option<String>,
    /// Срок.
    #[serde(default)]
    pub due: Option<String>,
}

/// Тело `POST /api/tracker/tasks/reorder`.
#[derive(Debug, Deserialize)]
pub struct ReorderReq {
    /// Идентификаторы в новом порядке. Задачи, которых в списке нет,
    /// уезжают в хвост, сохраняя относительный порядок.
    pub ids: Vec<String>,
}

/// Тело `PATCH /api/tracker/tasks/:id/status`.
#[derive(Debug, Deserialize)]
pub struct StatusReq {
    /// Новая колонка канбана.
    pub status: String,
}

/// `GET /api/tracker/tasks` — все задачи в порядке отображения.
pub async fn list_tasks(State(state): State<Arc<TrackerState>>) -> Json<Vec<TrackerTask>> {
    Json(state.store.tasks())
}

/// `GET /api/tracker/tasks/:id` — одна задача.
pub async fn get_task(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<TrackerTask>> {
    state
        .store
        .task(&id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("task {id}")))
}

/// `POST /api/tracker/tasks` — создать задачу.
pub async fn create_task(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<TrackerTask>)> {
    let req: CreateTaskReq = parse_body(body)?;
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    let status = match req.status.as_deref() {
        Some(raw) => Some(parse_status(raw)?),
        None => None,
    };

    let task = state.store.create_task(NewTask {
        title,
        list_id: req.list_id,
        notes: req.notes,
        status,
        priority: req.priority,
        tags: req.tags.unwrap_or_default(),
        estimate_min: req.estimate_min,
        start: req.start,
        due: req.due,
    })?;
    state.emit(TrackerEvent::task_upsert(task.clone()));
    Ok((axum::http::StatusCode::CREATED, Json(task)))
}

/// `PATCH /api/tracker/tasks/:id` — частичное обновление.
///
/// Незаданные ключи не трогаются, `null` у обнуляемых полей (`list_id`,
/// `notes`, `estimate_min`, `start`, `due`) СБРАСЫВАЕТ поле — три состояния
/// различает [`TaskPatch`].
pub async fn patch_task(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<TrackerTask>> {
    // Статус проверяется ДО общего разбора: иначе на неизвестной колонке
    // сообщение об ошибке было бы «unknown variant», без перечисления
    // допустимых значений.
    if let Some(raw) = body.get("status") {
        let raw = raw
            .as_str()
            .ok_or_else(|| ApiError::bad_request("status must be a string"))?;
        parse_status(raw)?;
    }
    let patch: TaskPatch = parse_body(body)?;
    let task = state.store.update_task(&id, patch)?;
    state.emit(TrackerEvent::task_upsert(task.clone()));
    Ok(Json(task))
}

/// `PATCH /api/tracker/tasks/:id/status` — перенос между колонками канбана.
///
/// Отдельный роут, а не частный случай общего патча: перетаскивание карточки —
/// самое частое действие в канбане, и оно не должно нести риск случайно
/// затереть соседние поля задачи телом запроса.
pub async fn set_task_status(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<TrackerTask>> {
    let req: StatusReq = parse_body(body)?;
    let status: TaskStatus = parse_status(&req.status)?;
    let task = state.store.set_status(&id, status)?;
    state.emit(TrackerEvent::task_upsert(task.clone()));
    Ok(Json(task))
}

/// `POST /api/tracker/tasks/reorder` — новый порядок задач.
pub async fn reorder_tasks(
    State(state): State<Arc<TrackerState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Vec<TrackerTask>>> {
    let req: ReorderReq = parse_body(body)?;
    state.store.reorder_tasks(&req.ids)?;
    state.emit(TrackerEvent::Reload);
    Ok(Json(state.store.tasks()))
}

/// `DELETE /api/tracker/tasks/:id` — удалить задачу.
///
/// Вместе с задачей уходят её пункты чек-листа и отрезки времени: отрезок без
/// задачи не имеет смысла ни в отчёте, ни на Гантте, а осиротевшие записи
/// молча искажали бы суммы.
pub async fn delete_task(
    State(state): State<Arc<TrackerState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.delete_task(&id)?;
    state.emit(TrackerEvent::task_removed(id.clone()));
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}
