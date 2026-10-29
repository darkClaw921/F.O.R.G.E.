//! Тесты HTTP-поверхности `/api/tracker/*`.
//!
//! Запросы идут через СОБРАННЫЙ роутер (`tower::ServiceExt::oneshot`), а не
//! мимо него в стор: проверяется ровно та таблица маршрутов, которую получит
//! хост, поэтому опечатка в пути или потерянный метод краснеют здесь, а не
//! молчаливым 404 у пользователя.
//!
//! Стор у каждого теста свой, во временном каталоге: живой
//! `~/.config/forge/tracker.json` прогон тестов не трогает НИКОГДА.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use forge_tracker::state::TrackerState;
use forge_tracker::{TrackerConfig, TrackerStore};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Изолированный плагин: временный стор, свой роутер.
fn app() -> (tempfile::TempDir, Arc<TrackerState>, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = TrackerConfig::with_store_path(dir.path().join("tracker.json"));
    let state = forge_tracker::init(cfg).expect("init");
    let router = forge_tracker::register_routes(Router::new(), state.clone());
    (dir, state, router)
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn create_task(router: &Router, title: &str) -> Value {
    let (status, task) = call(router, "POST", "/api/tracker/tasks", Some(json!({"title": title}))).await;
    assert_eq!(status, StatusCode::CREATED, "{task}");
    task
}

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("id in response").to_string()
}

// ---------------------------------------------------------------- гейт и снимок

#[tokio::test]
async fn status_is_200_when_routes_are_registered() {
    let (_dir, _state, router) = app();
    let (status, body) = call(&router, "GET", "/api/tracker/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["tasks"], json!(0));
    assert_eq!(body["active_timers"], json!(0));
}

#[tokio::test]
async fn unknown_tracker_path_is_404_not_a_panic() {
    let (_dir, _state, router) = app();
    let (status, _) = call(&router, "GET", "/api/tracker/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn state_carries_collections_and_aggregates() {
    let (_dir, _state, router) = app();
    let task = create_task(&router, "написать письмо").await;
    let task_id = id_of(&task);

    let (status, body) = call(&router, "GET", "/api/tracker/state", None).await;
    assert_eq!(status, StatusCode::OK);
    for key in [
        "lists",
        "tasks",
        "items",
        "milestones",
        "entries",
        "active",
        "task_seconds",
        "item_seconds",
        "progress",
        "totals",
        "now",
    ] {
        assert!(body.get(key).is_some(), "state has no {key}: {body}");
    }
    assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(body["task_seconds"][&task_id], json!(0));
    assert_eq!(body["progress"][&task_id]["total"], json!(0));
    // Задачи глобальные: ни следа корня проекта в ответе.
    let raw = body.to_string();
    assert!(!raw.contains("root_path"), "state leaks project scope");
}

// ------------------------------------------------------------------ задачи

#[tokio::test]
async fn created_task_carries_id_and_created_at() {
    let (_dir, _state, router) = app();
    let task = create_task(&router, "  починить кран  ").await;
    assert!(!id_of(&task).is_empty());
    assert_eq!(task["title"], json!("починить кран"), "title is trimmed");
    assert!(task["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(task["status"], json!("inbox"));
    assert_eq!(task["priority"], json!(2));
}

#[tokio::test]
async fn empty_title_is_rejected() {
    let (_dir, _state, router) = app();
    let (status, body) = call(
        &router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("bad_request"));
}

#[tokio::test]
async fn malformed_body_is_400_in_the_common_envelope() {
    let (_dir, _state, router) = app();
    let (status, body) = call(
        &router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": 42})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("bad_request"));
}

#[tokio::test]
async fn patch_is_partial_and_null_clears_the_field() {
    let (_dir, _state, router) = app();
    let task = create_task(&router, "созвон").await;
    let id = id_of(&task);

    let (status, patched) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{id}"),
        Some(json!({"notes": "с командой", "due": "2026-09-10", "estimate_min": 60})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Второй патч трогает ОДНО поле — остальные обязаны уцелеть.
    let (status, again) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{id}"),
        Some(json!({"priority": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["notes"], patched["notes"], "notes survived");
    assert_eq!(again["due"], json!("2026-09-10"), "due survived");
    assert_eq!(again["estimate_min"], json!(60), "estimate survived");
    assert_eq!(again["priority"], json!(0));
    assert_eq!(again["title"], json!("созвон"));

    // Явный null — сброс.
    let (status, cleared) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{id}"),
        Some(json!({"due": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["due"], json!(null));
    assert_eq!(cleared["estimate_min"], json!(60), "null did not spill over");
}

#[tokio::test]
async fn status_route_moves_between_kanban_columns() {
    let (_dir, _state, router) = app();
    let id = id_of(&create_task(&router, "переставить").await);

    for column in ["today", "doing", "waiting", "done", "inbox"] {
        let (status, body) = call(
            &router,
            "PATCH",
            &format!("/api/tracker/tasks/{id}/status"),
            Some(json!({ "status": column })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], json!(column));
    }
}

#[tokio::test]
async fn unknown_status_is_400_both_on_patch_and_on_the_status_route() {
    let (_dir, _state, router) = app();
    let id = id_of(&create_task(&router, "переставить").await);

    let (status, body) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{id}/status"),
        Some(json!({"status": "backlog"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("waiting"));

    let (status, _) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{id}"),
        Some(json!({"status": "backlog"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "422 from serde is not allowed");
}

#[tokio::test]
async fn delete_of_a_missing_task_is_404_not_500() {
    let (_dir, _state, router) = app();
    let (status, body) = call(&router, "DELETE", "/api/tracker/tasks/no-such-id", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("not_found"));

    let (status, _) = call(&router, "GET", "/api/tracker/tasks/no-such-id", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &router,
        "PATCH",
        "/api/tracker/tasks/no-such-id",
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reorder_sets_the_new_order() {
    let (_dir, _state, router) = app();
    let a = id_of(&create_task(&router, "a").await);
    let b = id_of(&create_task(&router, "b").await);
    let c = id_of(&create_task(&router, "c").await);

    let (status, tasks) = call(
        &router,
        "POST",
        "/api/tracker/tasks/reorder",
        Some(json!({ "ids": [c, a, b] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let titles: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["c", "a", "b"]);
}

// ------------------------------------------------------------------ таймеры

#[tokio::test]
async fn timers_run_in_parallel_across_tasks() {
    let (_dir, _state, router) = app();
    let a = id_of(&create_task(&router, "созвон").await);
    let b = id_of(&create_task(&router, "правка кода").await);

    let (sa, ea) = call(&router, "POST", &format!("/api/tracker/tasks/{a}/timer/start"), None).await;
    let (sb, eb) = call(&router, "POST", &format!("/api/tracker/tasks/{b}/timer/start"), None).await;
    assert_eq!(sa, StatusCode::OK);
    assert_eq!(sb, StatusCode::OK);
    assert_ne!(id_of(&ea), id_of(&eb));

    let (_, state) = call(&router, "GET", "/api/tracker/state", None).await;
    assert_eq!(
        state["active"].as_array().unwrap().len(),
        2,
        "оба таймера идут: {}",
        state["active"]
    );

    // Остановка одного не трогает второй.
    let (status, stopped) =
        call(&router, "POST", &format!("/api/tracker/tasks/{a}/timer/stop"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["stopped"], json!(true));

    let (_, state) = call(&router, "GET", "/api/tracker/state", None).await;
    let active = state["active"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["task_id"], json!(b));
}

#[tokio::test]
async fn repeated_start_returns_the_same_entry() {
    let (_dir, _state, router) = app();
    let id = id_of(&create_task(&router, "созвон").await);
    let uri = format!("/api/tracker/tasks/{id}/timer/start");

    let (_, first) = call(&router, "POST", &uri, None).await;
    let (_, second) = call(&router, "POST", &uri, Some(json!({"note": "ещё раз"}))).await;
    assert_eq!(id_of(&first), id_of(&second), "второй start не плодит отрезки");

    let (_, entries) = call(&router, "GET", "/api/tracker/entries", None).await;
    assert_eq!(entries.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn stopping_an_idle_task_is_ok_and_says_so() {
    let (_dir, _state, router) = app();
    let id = id_of(&create_task(&router, "созвон").await);
    let (status, body) =
        call(&router, "POST", &format!("/api/tracker/tasks/{id}/timer/stop"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stopped"], json!(false));
}

#[tokio::test]
async fn timer_of_a_missing_task_is_404() {
    let (_dir, _state, router) = app();
    let (status, _) = call(&router, "POST", "/api/tracker/tasks/ghost/timer/start", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&router, "POST", "/api/tracker/tasks/ghost/timer/stop", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------ ручные отрезки

#[tokio::test]
async fn manual_entry_crud() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "вчерашняя работа").await);

    let (status, entry) = call(
        &router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T09:00:00Z",
            "stopped_at": "2026-09-07T10:30:00Z",
            "note": "разбор почты",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    assert_eq!(entry["source"], json!("manual"));
    let entry_id = id_of(&entry);

    let (status, patched) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/entries/{entry_id}"),
        Some(json!({"stopped_at": "2026-09-07T11:00:00Z"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["stopped_at"], json!("2026-09-07T11:00:00Z"));
    assert_eq!(patched["note"], json!("разбор почты"), "note survived");

    let (status, listed) = call(
        &router,
        "GET",
        &format!("/api/tracker/entries?task_id={task_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/entries/{entry_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = call(&router, "GET", "/api/tracker/entries", None).await;
    assert!(listed.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn inverted_entry_is_400_on_create_and_on_patch() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "правка").await);

    let (status, body) = call(
        &router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T10:00:00Z",
            "stopped_at": "2026-09-07T09:00:00Z",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (_, entry) = call(
        &router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T09:00:00Z",
            "stopped_at": "2026-09-07T10:00:00Z",
        })),
    )
    .await;
    let entry_id = id_of(&entry);

    // Патч двигает ТОЛЬКО конец — проверка обязана смотреть на итог.
    let (status, body) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/entries/{entry_id}"),
        Some(json!({"stopped_at": "2026-09-07T08:00:00Z"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("bad_request"));

    // И симметрично — когда двигают начало.
    let (status, _) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/entries/{entry_id}"),
        Some(json!({"started_at": "2026-09-07T23:00:00Z"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/entries/{entry_id}"),
        Some(json!({"started_at": "позавчера"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn second_running_entry_on_the_same_task_is_409() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "созвон").await);
    let (_, _) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/timer/start"),
        None,
    )
    .await;

    let (status, body) = call(
        &router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T09:00:00Z",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("conflict"));
}

// ------------------------------------------------------------------ списки

#[tokio::test]
async fn list_crud_and_deleting_a_list_keeps_its_tasks() {
    let (_dir, _state, router) = app();
    let (status, list) = call(
        &router,
        "POST",
        "/api/tracker/lists",
        Some(json!({"name": "дом", "color": "#C98BFF"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let list_id = id_of(&list);

    let (_, task) = call(
        &router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "полить цветы", "list_id": list_id})),
    )
    .await;
    let task_id = id_of(&task);

    let (status, renamed) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/lists/{list_id}"),
        Some(json!({"name": "быт"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], json!("быт"));
    assert_eq!(renamed["color"], json!("#C98BFF"), "color survived");

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/lists/{list_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, task) = call(&router, "GET", &format!("/api/tracker/tasks/{task_id}"), None).await;
    assert_eq!(task["title"], json!("полить цветы"), "task survived");
    assert_eq!(task["list_id"], json!(null), "task fell back to inbox");

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/lists/{list_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "второе удаление — 404");
}

#[tokio::test]
async fn lists_reorder() {
    let (_dir, _state, router) = app();
    let mut ids = Vec::new();
    for name in ["a", "b", "c"] {
        let (_, list) = call(
            &router,
            "POST",
            "/api/tracker/lists",
            Some(json!({ "name": name })),
        )
        .await;
        ids.push(id_of(&list));
    }
    let (status, lists) = call(
        &router,
        "POST",
        "/api/tracker/lists/reorder",
        Some(json!({"ids": [ids[2], ids[0], ids[1]]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = lists
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["c", "a", "b"]);
}

// ---------------------------------------------------------------- чек-лист

#[tokio::test]
async fn items_are_returned_in_order_and_scoped_to_their_task() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "переезд").await);
    let other_id = id_of(&create_task(&router, "чужая задача").await);

    let mut item_ids = Vec::new();
    for title in ["коробки", "машина", "ключи"] {
        let (status, item) = call(
            &router,
            "POST",
            &format!("/api/tracker/tasks/{task_id}/items"),
            Some(json!({ "title": title })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        item_ids.push(id_of(&item));
    }

    let (status, items) = call(
        &router,
        "GET",
        &format!("/api/tracker/tasks/{task_id}/items"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let titles: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["коробки", "машина", "ключи"]);

    let (status, items) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/items/reorder"),
        Some(json!({"ids": [item_ids[2], item_ids[1], item_ids[0]]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let titles: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["ключи", "машина", "коробки"]);

    // Чужой пункт через свою задачу недоступен.
    let (status, _) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/tasks/{other_id}/items/{}", item_ids[0]),
        Some(json!({"title": "подмена"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn items_of_a_missing_task_are_404() {
    let (_dir, _state, router) = app();
    let (status, _) = call(&router, "GET", "/api/tracker/tasks/ghost/items", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &router,
        "POST",
        "/api/tracker/tasks/ghost/items",
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &router,
        "POST",
        "/api/tracker/tasks/ghost/items/also-ghost/timer/start",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn toggle_moves_progress_and_the_item_timer_feeds_the_task_total() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "переезд").await);
    let mut items = Vec::new();
    for title in ["коробки", "машина"] {
        let (_, item) = call(
            &router,
            "POST",
            &format!("/api/tracker/tasks/{task_id}/items"),
            Some(json!({ "title": title })),
        )
        .await;
        items.push(id_of(&item));
    }

    let (status, toggled) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/items/{}/toggle", items[0]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(toggled["done"], json!(true));
    assert!(toggled["done_at"].is_string());

    let (_, state) = call(&router, "GET", "/api/tracker/state", None).await;
    assert_eq!(state["progress"][&task_id]["done"], json!(1));
    assert_eq!(state["progress"][&task_id]["total"], json!(2));
    assert_eq!(state["progress"][&task_id]["percent"], json!(50));

    // Таймер пункта: отрезок несёт item_id и виден в общем состоянии.
    let (status, entry) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/items/{}/timer/start", items[1]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry["item_id"], json!(items[1]));
    assert_eq!(entry["task_id"], json!(task_id));

    let (_, state) = call(&router, "GET", "/api/tracker/state", None).await;
    let active = state["active"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["item_id"], json!(items[1]));

    // И останавливается он тем же путём.
    let (status, stopped) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/items/{}/timer/stop", items[1]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["stopped"], json!(true));

    // Пункт удаляем — время задачи остаётся: оно потрачено.
    let (_, before) = call(&router, "GET", "/api/tracker/entries", None).await;
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/tasks/{task_id}/items/{}", items[1]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = call(&router, "GET", "/api/tracker/entries", None).await;
    assert_eq!(
        before.as_array().unwrap().len(),
        after.as_array().unwrap().len()
    );
}

// -------------------------------------------------------------------- вехи

#[tokio::test]
async fn milestone_crud_and_date_validation() {
    let (_dir, _state, router) = app();
    let (status, ms) = call(
        &router,
        "POST",
        "/api/tracker/milestones",
        Some(json!({"title": "релиз", "date": "2026-09-10"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let ms_id = id_of(&ms);

    let (status, body) = call(
        &router,
        "POST",
        "/api/tracker/milestones",
        Some(json!({"title": "релиз", "date": "когда-нибудь"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, patched) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/milestones/{ms_id}"),
        Some(json!({"date": "2026-09-11"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["date"], json!("2026-09-11"));

    let (status, _) = call(
        &router,
        "PATCH",
        &format!("/api/tracker/milestones/{ms_id}"),
        Some(json!({"date": "нет"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, listed) = call(&router, "GET", "/api/tracker/milestones", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/milestones/{ms_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/tracker/milestones/{ms_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------- отчёт и Гантт

#[tokio::test]
async fn report_defaults_to_the_current_week_and_validates_input() {
    let (_dir, _state, router) = app();
    let task_id = id_of(&create_task(&router, "работа").await);
    let (_, _) = call(
        &router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/timer/start"),
        None,
    )
    .await;

    let (status, body) = call(&router, "GET", "/api/tracker/report", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["group_by"], json!("day"));
    assert!(body["from"].is_string(), "окно по умолчанию заполнено");
    assert!(body["to"].is_string());

    // Шапка сводки (KPI экрана отчёта) приезжает В ТОМ ЖЕ ответе: считать её
    // на фронте значило бы завести вторую редакцию правила суток.
    let kpi = &body["kpi"];
    assert!(kpi.is_object(), "ответ несёт kpi: {body}");
    assert_eq!(kpi["total_seconds"], body["total_seconds"]);
    assert!(kpi["days"].as_i64().unwrap_or(0) >= 1, "делить на дни безопасно");
    for field in [
        "avg_day_seconds",
        "closed_tasks",
        "touched_tasks",
        "max_focus_seconds",
    ] {
        assert!(kpi[field].is_i64(), "поле {field} на месте: {kpi}");
    }
    // Секунд у только что запущенного таймера ещё нет, поэтому «максимальный
    // фокус» здесь честный ноль; что идущий отрезок считается до `now`,
    // проверяет тест расчёта в report.rs, где `now` задан явно.
    assert_eq!(kpi["max_focus_seconds"], json!(0));
    assert!(kpi["max_focus_task_id"].is_null());

    for group_by in ["day", "task", "list"] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/api/tracker/report?group_by={group_by}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["group_by"], json!(group_by));
    }

    let (status, body) = call(&router, "GET", "/api/tracker/report?group_by=week", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("day"));

    let (status, _) = call(&router, "GET", "/api/tracker/report?from=вчера", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        &router,
        "GET",
        "/api/tracker/report?from=2026-09-10&to=2026-09-01",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn gantt_carries_bars_over_estimate_flag_and_milestones() {
    let (_dir, _state, router) = app();
    let (_, task) = call(
        &router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({
            "title": "долгая задача",
            "start": "2026-09-07",
            "due": "2026-09-09",
            "estimate_min": 1,
        })),
    )
    .await;
    let task_id = id_of(&task);
    let (_, _) = call(
        &router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T09:00:00Z",
            "stopped_at": "2026-09-07T13:38:00Z",
        })),
    )
    .await;
    let (_, _) = call(
        &router,
        "POST",
        "/api/tracker/milestones",
        Some(json!({"title": "сдача", "date": "2026-09-08"})),
    )
    .await;

    let (status, body) = call(
        &router,
        "GET",
        "/api/tracker/gantt?from=2026-09-07&to=2026-09-14",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let bar = &body["bars"][0];
    assert_eq!(bar["task_id"], json!(task_id));
    assert_eq!(bar["over_estimate"], json!(true), "1 минута против 4:38");
    assert!(bar["actual_seconds"].as_i64().unwrap() > 16000);
    assert_eq!(body["milestones"].as_array().unwrap().len(), 1);

    let (status, _) = call(&router, "GET", "/api/tracker/gantt?to=никогда", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------------- изоляция

#[tokio::test]
async fn every_mutation_lands_on_disk_in_the_temp_store() {
    let (dir, _state, router) = app();
    let path = dir.path().join("tracker.json");
    let task_id = id_of(&create_task(&router, "сохранись").await);

    assert!(path.exists(), "мутация записала файл");
    let reopened = TrackerStore::load(&path).expect("reload");
    assert_eq!(reopened.tasks().len(), 1);
    assert_eq!(reopened.tasks()[0].id, task_id);
}

/// Сторож замороженной таблицы путей.
///
/// Стучится по КАЖДОМУ пути из [`forge_tracker::routes::ROUTE_PATHS`] всеми
/// четырьмя методами и требует, чтобы хотя бы один из них дошёл до обработчика.
/// Признак «маршрута нет» отличается от «объект не найден» телом ответа: наш
/// 404 всегда несёт конверт `{"error":{...}}`, а 404 самого axum — пустое тело.
/// Без этой проверки список путей был бы просто комментарием, который
/// разъезжается с роутером молча.
#[tokio::test]
async fn every_frozen_route_is_registered() {
    let (_dir, _state, router) = app();
    for path in forge_tracker::routes::ROUTE_PATHS {
        let uri = path.replace(":item_id", "ghost-item").replace(":id", "ghost");
        let mut reachable = false;
        for method in ["GET", "POST", "PATCH", "DELETE"] {
            let (status, body) = call(&router, method, &uri, Some(json!({}))).await;
            let route_missing = status == StatusCode::NOT_FOUND && body.get("error").is_none();
            if route_missing || status == StatusCode::METHOD_NOT_ALLOWED {
                continue;
            }
            reachable = true;
            break;
        }
        assert!(reachable, "route {path} is not registered in build_router");
    }
}
