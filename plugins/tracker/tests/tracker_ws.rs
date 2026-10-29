//! Тесты realtime-потока `/ws/tracker`.
//!
//! Соединение поднимается НАСТОЯЩЕЕ: сервер слушает эфемерный порт
//! `127.0.0.1:0`, клиент подключается обычным `tokio-tungstenite`. Имитация
//! (позвать handler напрямую) проверяла бы форму кадров, но не проверила бы
//! главного — что маршрут `/ws/tracker` вообще отвечает на upgrade. Ровно эта
//! ошибка проявилась бы у пользователя как «панель не обновляется», без
//! единого признака в логах сервера.
//!
//! Мутации во время теста делаются через тот же роутер (`oneshot`), а не мимо
//! него: событие обязано приходить именно от REST-роута, иначе тест доказывал
//! бы работоспособность канала, а не проводки роут → канал → клиент.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use forge_tracker::state::TrackerState;
use forge_tracker::ws::TrackerEvent;
use forge_tracker::TrackerConfig;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Сколько ждать кадр, прежде чем считать тест провалившимся.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    _dir: tempfile::TempDir,
    state: Arc<TrackerState>,
    router: Router,
    url: String,
}

/// Поднимает плагин на временном сторе и сервер на свободном порту.
async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = TrackerConfig::with_store_path(dir.path().join("tracker.json"));
    let state = forge_tracker::init(cfg).expect("init");
    let router = forge_tracker::register_routes(Router::new(), state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("addr");
    let served = router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served.into_make_service()).await;
    });

    Harness {
        _dir: dir,
        state,
        router,
        url: format!("ws://{addr}/ws/tracker"),
    }
}

async fn http(router: &Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.expect("router call");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Следующий текстовый кадр как JSON. Ping/Pong пропускаются.
async fn next_frame<S>(socket: &mut S) -> Value
where
    S: StreamExt<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    loop {
        let msg = tokio::time::timeout(FRAME_TIMEOUT, socket.next())
            .await
            .expect("frame arrived in time")
            .expect("stream is alive")
            .expect("frame is not an error");
        if let tokio_tungstenite::tungstenite::Message::Text(raw) = msg {
            return serde_json::from_str(&raw).expect("frame is JSON");
        }
    }
}

#[tokio::test]
async fn connect_yields_a_snapshot_first() {
    let h = harness().await;
    // Задача, заведённая ДО подключения, обязана быть в снимке.
    http(
        &h.router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "до подключения"})),
    )
    .await;

    let (mut socket, _) = tokio_tungstenite::connect_async(&h.url)
        .await
        .expect("ws connect");
    let frame = next_frame(&mut socket).await;

    assert_eq!(frame["kind"], json!("snapshot"));
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
        assert!(frame.get(key).is_some(), "snapshot has no {key}: {frame}");
    }
    assert_eq!(frame["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(frame["tasks"][0]["title"], json!("до подключения"));
}

#[tokio::test]
async fn rest_mutations_reach_the_socket() {
    let h = harness().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(&h.url)
        .await
        .expect("ws connect");
    assert_eq!(next_frame(&mut socket).await["kind"], json!("snapshot"));

    // Создание задачи.
    let task = http(
        &h.router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "созвон"})),
    )
    .await;
    let task_id = task["id"].as_str().unwrap().to_string();
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("task_upsert"));
    assert_eq!(frame["task"]["id"], json!(task_id));
    assert_eq!(frame["task"]["title"], json!("созвон"));

    // Старт таймера.
    http(
        &h.router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/timer/start"),
        None,
    )
    .await;
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("entry_upsert"));
    assert_eq!(frame["entry"]["task_id"], json!(task_id));
    assert_eq!(frame["entry"]["stopped_at"], json!(null));

    // Смена колонки канбана.
    http(
        &h.router,
        "PATCH",
        &format!("/api/tracker/tasks/{task_id}/status"),
        Some(json!({"status": "doing"})),
    )
    .await;
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("task_upsert"));
    assert_eq!(frame["task"]["status"], json!("doing"));

    // Удаление задачи. Кадр удаления отрезков не шлётся отдельно: они уходят
    // вместе с задачей, и клиент чистит их по task_id.
    http(
        &h.router,
        "DELETE",
        &format!("/api/tracker/tasks/{task_id}"),
        None,
    )
    .await;
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("task_removed"));
    assert_eq!(frame["id"], json!(task_id));
}

#[tokio::test]
async fn manual_entry_and_list_mutations_have_their_own_frames() {
    let h = harness().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(&h.url)
        .await
        .expect("ws connect");
    assert_eq!(next_frame(&mut socket).await["kind"], json!("snapshot"));

    let task = http(
        &h.router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "вчера"})),
    )
    .await;
    assert_eq!(next_frame(&mut socket).await["kind"], json!("task_upsert"));
    let task_id = task["id"].as_str().unwrap().to_string();

    let entry = http(
        &h.router,
        "POST",
        "/api/tracker/entries",
        Some(json!({
            "task_id": task_id,
            "started_at": "2026-09-07T09:00:00Z",
            "stopped_at": "2026-09-07T10:00:00Z",
        })),
    )
    .await;
    let entry_id = entry["id"].as_str().unwrap().to_string();
    assert_eq!(next_frame(&mut socket).await["kind"], json!("entry_upsert"));

    http(
        &h.router,
        "DELETE",
        &format!("/api/tracker/entries/{entry_id}"),
        None,
    )
    .await;
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("entry_removed"));
    assert_eq!(frame["id"], json!(entry_id));

    // У списков точечного кадра нет — приходит reload.
    http(
        &h.router,
        "POST",
        "/api/tracker/lists",
        Some(json!({"name": "дом"})),
    )
    .await;
    assert_eq!(next_frame(&mut socket).await["kind"], json!("reload"));
}

#[tokio::test]
async fn checklist_changes_refresh_the_parent_task() {
    let h = harness().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(&h.url)
        .await
        .expect("ws connect");
    assert_eq!(next_frame(&mut socket).await["kind"], json!("snapshot"));

    let task = http(
        &h.router,
        "POST",
        "/api/tracker/tasks",
        Some(json!({"title": "переезд"})),
    )
    .await;
    let task_id = task["id"].as_str().unwrap().to_string();
    assert_eq!(next_frame(&mut socket).await["kind"], json!("task_upsert"));

    http(
        &h.router,
        "POST",
        &format!("/api/tracker/tasks/{task_id}/items"),
        Some(json!({"title": "коробки"})),
    )
    .await;
    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["kind"], json!("task_upsert"));
    assert_eq!(frame["task"]["id"], json!(task_id));
}

#[tokio::test]
async fn a_lagging_subscriber_is_recovered_rather_than_dropped() {
    let h = harness().await;
    let mut rx = h.state.events.subscribe();

    // Буфер канала — 64. Переполняем его заведомо: подписчик, который не
    // читал, обязан узнать об этом кодом Lagged, а не тишиной.
    for i in 0..200 {
        h.state.emit(TrackerEvent::task_removed(format!("t{i}")));
    }

    let err = rx.try_recv().expect_err("subscriber must be reported as lagged");
    assert!(
        matches!(
            err,
            tokio::sync::broadcast::error::TryRecvError::Lagged(_)
        ),
        "unexpected error: {err:?}"
    );

    // Ровно на этот случай handler отправляет клиенту новый снимок: он
    // самодостаточен и приводит клиента в согласованное состояние.
    let frame = forge_tracker::ws::snapshot_frame(&h.state);
    assert_eq!(frame["kind"], json!("snapshot"));
    assert!(frame.get("tasks").is_some());
}
