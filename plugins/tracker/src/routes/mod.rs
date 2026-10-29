//! HTTP-поверхность плагина `forge-tracker`.
//!
//! Всё живёт под префиксом `/api/tracker/*` плюс WebSocket `/ws/tracker` —
//! коллизий с роутами хоста нет по построению. Сборка идёт в
//! [`build_router`], который зовёт [`crate::register_routes`] ДО
//! `.fallback(...)` и `.layer(auth)` хоста: только так Bearer-auth и
//! csrf_guard автоматически покрывают роуты плагина в remote-mode.
//!
//! # ЗАМОРОЖЕННЫЙ КОНТРАКТ ИМЁН
//!
//! Таблица ниже — источник истины для `tmux-web/static/js/tracker/api.js`
//! (фаза 4). Обе стороны обязаны совпадать дословно: расхождение даёт
//! молчаливый 404, а не ошибку сборки.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/tracker/status` | гейт: плагин включён (без флага хост роуты не регистрирует вовсе → 404) |
//! | GET | `/api/tracker/state` | полный снимок: списки, задачи, чек-листы, вехи, отрезки, агрегаты |
//! | GET | `/api/tracker/tasks` | задачи в порядке отображения |
//! | POST | `/api/tracker/tasks` | создать задачу |
//! | POST | `/api/tracker/tasks/reorder` | новый порядок задач |
//! | GET | `/api/tracker/tasks/:id` | одна задача |
//! | PATCH | `/api/tracker/tasks/:id` | частичное обновление |
//! | DELETE | `/api/tracker/tasks/:id` | удалить задачу (с её пунктами и отрезками) |
//! | PATCH | `/api/tracker/tasks/:id/status` | перенос между колонками канбана |
//! | POST | `/api/tracker/tasks/:id/timer/start` | запустить таймер задачи |
//! | POST | `/api/tracker/tasks/:id/timer/stop` | остановить таймер задачи |
//! | GET | `/api/tracker/tasks/:id/items` | пункты чек-листа |
//! | POST | `/api/tracker/tasks/:id/items` | добавить пункт |
//! | POST | `/api/tracker/tasks/:id/items/reorder` | новый порядок пунктов |
//! | PATCH | `/api/tracker/tasks/:id/items/:item_id` | правка пункта |
//! | DELETE | `/api/tracker/tasks/:id/items/:item_id` | удалить пункт |
//! | POST | `/api/tracker/tasks/:id/items/:item_id/toggle` | переключить галочку |
//! | POST | `/api/tracker/tasks/:id/items/:item_id/timer/start` | таймер пункта |
//! | POST | `/api/tracker/tasks/:id/items/:item_id/timer/stop` | остановить таймер пункта |
//! | GET | `/api/tracker/entries` | отрезки времени (`?task_id=`) |
//! | POST | `/api/tracker/entries` | ручной отрезок задним числом |
//! | PATCH | `/api/tracker/entries/:id` | правка отрезка |
//! | DELETE | `/api/tracker/entries/:id` | удалить отрезок |
//! | GET | `/api/tracker/lists` | списки |
//! | POST | `/api/tracker/lists` | создать список |
//! | POST | `/api/tracker/lists/reorder` | новый порядок списков |
//! | PATCH | `/api/tracker/lists/:id` | правка списка |
//! | DELETE | `/api/tracker/lists/:id` | удалить список (задачи остаются) |
//! | GET | `/api/tracker/milestones` | вехи Гантта |
//! | POST | `/api/tracker/milestones` | создать веху |
//! | PATCH | `/api/tracker/milestones/:id` | правка вехи |
//! | DELETE | `/api/tracker/milestones/:id` | удалить веху |
//! | GET | `/api/tracker/report` | сводка (`?from&to&group_by=day\|task\|list`) |
//! | GET | `/api/tracker/gantt` | полосы и вехи (`?from&to`) |
//! | GET | `/ws/tracker` | поток событий (см. [`crate::ws`]) |
//!
//! # Три решения, зафиксированные здесь
//!
//! 1. **Ни одного `path` или `root_path` в запросах.** Задачи трекера
//!    ГЛОБАЛЬНЫЕ; фильтр по корню проекта, обязательный для `/api/todos`,
//!    здесь отсутствует не по недосмотру, а по модели данных. Выбор корня
//!    случается ровно один раз — в момент копирования задачи в TODO (фаза 8),
//!    и делает его фронт.
//! 2. **Перестановка — `POST .../reorder`, а не череда `PATCH`.** Порядок это
//!    свойство КОЛЛЕКЦИИ: перетаскивание одной карточки меняет `order` у всех
//!    соседей, и разложить это на отдельные патчи означало бы гарантированную
//!    рассинхронизацию при обрыве на середине.
//! 3. **Тела разбираются через [`parse_body`], а не экстрактором `Json<T>`.**
//!    Причина — единый контракт ошибок: автоматический экстрактор ответил бы
//!    на кривое тело кодом 422 и телом axum, мимо формы
//!    `{"error":{code,message}}`, которую знает фронт.

pub mod entries;
pub mod error;
pub mod items;
pub mod lists;
pub mod milestones;
pub mod report;
pub mod tasks;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::dto::StateDto;
use crate::models::TaskStatus;
use crate::state::TrackerState;
use error::{ApiError, ApiResult};

/// Переводит уже разобранный JSON в типизированное тело запроса.
///
/// Ошибка формы — всегда 400 в общем конверте ошибок плагина.
pub fn parse_body<T: DeserializeOwned>(body: Value) -> ApiResult<T> {
    serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("invalid request body: {e}")))
}

/// Разбирает колонку канбана. 400 с перечислением допустимых значений.
pub fn parse_status(raw: &str) -> ApiResult<TaskStatus> {
    TaskStatus::parse(raw.trim()).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown status {raw:?}, expected one of: inbox, today, doing, waiting, done"
        ))
    })
}

/// `GET /api/tracker/status` — гейт плагина.
///
/// Отвечает 200 всегда, когда роуты вообще зарегистрированы. Выключенный
/// плагин даёт 404 не телом ответа, а ОТСУТСТВИЕМ роута: хост без флага
/// `--tracker` не вызывает [`crate::register_routes`] вовсе (фаза 3). Так гейт
/// невозможно случайно «включить» правкой одного условия внутри плагина.
pub async fn status(State(state): State<Arc<TrackerState>>) -> Json<Value> {
    let file = state.store.snapshot();
    Json(serde_json::json!({
        "enabled": true,
        "store_path": state.store.file_path().display().to_string(),
        "tasks": file.tasks.len(),
        "lists": file.lists.len(),
        "entries": file.entries.len(),
        "active_timers": file.entries.iter().filter(|e| e.stopped_at.is_none()).count(),
    }))
}

/// `GET /api/tracker/state` — полный снимок для панели.
///
/// Та же форма, что у кадра `snapshot` в `/ws/tracker` (общий [`StateDto`]).
pub async fn state_snapshot(State(state): State<Arc<TrackerState>>) -> Json<StateDto> {
    Json(StateDto::from_store(&state.store))
}

/// Собирает роутер плагина.
pub fn build_router(state: Arc<TrackerState>) -> Router {
    Router::new()
        .route("/api/tracker/status", get(status))
        .route("/api/tracker/state", get(state_snapshot))
        // --- задачи
        .route(
            "/api/tracker/tasks",
            get(tasks::list_tasks).post(tasks::create_task),
        )
        .route("/api/tracker/tasks/reorder", post(tasks::reorder_tasks))
        .route(
            "/api/tracker/tasks/:id",
            get(tasks::get_task)
                .patch(tasks::patch_task)
                .delete(tasks::delete_task),
        )
        .route("/api/tracker/tasks/:id/status", patch(tasks::set_task_status))
        // --- таймеры задачи
        .route(
            "/api/tracker/tasks/:id/timer/start",
            post(entries::start_timer),
        )
        .route(
            "/api/tracker/tasks/:id/timer/stop",
            post(entries::stop_timer),
        )
        // --- чек-лист
        .route(
            "/api/tracker/tasks/:id/items",
            get(items::list_items).post(items::create_item),
        )
        .route(
            "/api/tracker/tasks/:id/items/reorder",
            post(items::reorder_items),
        )
        .route(
            "/api/tracker/tasks/:id/items/:item_id",
            patch(items::patch_item).delete(items::delete_item),
        )
        .route(
            "/api/tracker/tasks/:id/items/:item_id/toggle",
            post(items::toggle_item),
        )
        .route(
            "/api/tracker/tasks/:id/items/:item_id/timer/start",
            post(items::start_item_timer),
        )
        .route(
            "/api/tracker/tasks/:id/items/:item_id/timer/stop",
            post(items::stop_item_timer),
        )
        // --- отрезки времени
        .route(
            "/api/tracker/entries",
            get(entries::list_entries).post(entries::create_entry),
        )
        .route(
            "/api/tracker/entries/:id",
            patch(entries::patch_entry).delete(entries::delete_entry),
        )
        // --- списки
        .route(
            "/api/tracker/lists",
            get(lists::list_lists).post(lists::create_list),
        )
        .route("/api/tracker/lists/reorder", post(lists::reorder_lists))
        .route(
            "/api/tracker/lists/:id",
            patch(lists::patch_list).delete(lists::delete_list),
        )
        // --- вехи
        .route(
            "/api/tracker/milestones",
            get(milestones::list_milestones).post(milestones::create_milestone),
        )
        .route(
            "/api/tracker/milestones/:id",
            patch(milestones::patch_milestone).delete(milestones::delete_milestone),
        )
        // --- отчёты
        .route("/api/tracker/report", get(report::report))
        .route("/api/tracker/gantt", get(report::gantt))
        // --- realtime
        .route("/ws/tracker", get(crate::ws::tracker_ws))
        .with_state(state)
}

/// Пути роутера, ЗАМОРОЖЕННЫЕ вместе с таблицей в шапке модуля.
///
/// Список существует ради сторожевого теста
/// (`tests/tracker_routes.rs::every_frozen_route_is_registered`), который
/// стучится по КАЖДОМУ пути в собранный роутер. Он ловит и опечатку в пути, и
/// потерю роута при рефакторинге — то есть ровно те две беды, которые в
/// остальном проявились бы молчаливым 404 у пользователя, а не у сборки.
pub const ROUTE_PATHS: &[&str] = &[
    "/api/tracker/status",
    "/api/tracker/state",
    "/api/tracker/tasks",
    "/api/tracker/tasks/reorder",
    "/api/tracker/tasks/:id",
    "/api/tracker/tasks/:id/status",
    "/api/tracker/tasks/:id/timer/start",
    "/api/tracker/tasks/:id/timer/stop",
    "/api/tracker/tasks/:id/items",
    "/api/tracker/tasks/:id/items/reorder",
    "/api/tracker/tasks/:id/items/:item_id",
    "/api/tracker/tasks/:id/items/:item_id/toggle",
    "/api/tracker/tasks/:id/items/:item_id/timer/start",
    "/api/tracker/tasks/:id/items/:item_id/timer/stop",
    "/api/tracker/entries",
    "/api/tracker/entries/:id",
    "/api/tracker/lists",
    "/api/tracker/lists/reorder",
    "/api/tracker/lists/:id",
    "/api/tracker/milestones",
    "/api/tracker/milestones/:id",
    "/api/tracker/report",
    "/api/tracker/gantt",
    "/ws/tracker",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_paths_have_no_duplicates_and_no_project_scope() {
        let mut seen = std::collections::BTreeSet::new();
        for path in ROUTE_PATHS {
            assert!(seen.insert(*path), "duplicate route {path}");
            // Задачи трекера глобальные: сегмента корня проекта в путях быть
            // не может ни при каком расширении.
            assert!(!path.contains("root"), "{path} leaks project scope");
            assert!(!path.contains(":path"), "{path} leaks project scope");
        }
        assert_eq!(ROUTE_PATHS.len(), 24);
    }

    #[test]
    fn statuses_are_parsed_case_by_case() {
        for raw in ["inbox", "today", "doing", "waiting", "done"] {
            assert!(parse_status(raw).is_ok(), "{raw}");
        }
        let err = parse_status("backlog").expect_err("unknown column must be rejected");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("waiting"), "{}", err.message);
    }
}
