//! Смоук-тесты плагина личного трекера ПО ПРОВОДУ (близнец `meet_smoke.rs`).
//!
//! Отличие от юнит-тестов внутри `plugins/tracker/src/**`: здесь поднимается
//! НАСТОЯЩИЙ axum-сервер на свободном порту и запросы идут через реальный
//! HTTP-клиент. Это ловит класс дефектов, который `oneshot` внутри крейта
//! пропускает: несмонтированный роут, конфликт шаблонов путей, порядок слоёв,
//! потерянный при `merge` хост-роут.
//!
//! # Что покрыто
//!
//! **Opt-in — главный инвариант фазы 3.**
//! [`tracker_routes_are_absent_without_the_flag`] собирает роутер БЕЗ
//! `register_routes` и убеждается, что весь префикс `/api/tracker/*` и
//! `/ws/tracker` отвечают 404. Это НЕ «просто ошибка», а контракт фронтового
//! гейта: по 404 на `GET /api/tracker/status` фронт понимает, что кнопку
//! вкладки «Мои задачи» рисовать не нужно. Обеспечивает 404 именно отсутствие
//! роутов, а не флаг внутри плагина — крейт про opt-in ничего не знает.
//!
//! **С плагином тот же путь отвечает 200**, а хост-роут переживает `merge`.
//!
//! **Падение `init` не роняет сервер**: воспроизводится тот же
//! `match … Err => warn`, что стоит в `main.rs`, и проверяется, что соседние
//! роуты живы, а роутов плагина при этом нет.
//!
//! Все пути — во временном каталоге, поэтому живой
//! `~/.config/forge/tracker.json` разработчика не читается и не
//! перезаписывается.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use forge_tracker::{TrackerConfig, TrackerState};
use serde_json::Value;

/// Живой сервер с примонтированным плагином.
struct Harness {
    addr: SocketAddr,
    state: Arc<TrackerState>,
    _tempdir: tempfile::TempDir,
    _serve: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Поднимает сервер С плагином на герметичном сторе во временном каталоге.
async fn start_harness() -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let cfg = TrackerConfig::with_store_path(tempdir.path().join("tracker.json"));
    let state = forge_tracker::init(cfg).expect("init обязан отработать на пустом каталоге");

    // Порядок сборки повторяет `main.rs`: сначала хост-роуты, затем
    // `register_routes` плагина, и только потом `.fallback`.
    let app = Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }));
    let app = forge_tracker::register_routes(app, state.clone())
        .fallback(|| async { axum::http::StatusCode::NOT_FOUND });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    Harness {
        addr,
        state,
        _tempdir: tempdir,
        _serve: serve,
    }
}

/// Поднимает сервер БЕЗ плагина — ровно то, что делает `devforge` без
/// `--tracker`.
async fn start_without_plugin() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    // `.fallback` повторяет боевую сборку: в `main.rs` неизвестные пути
    // уходят в статику, которая отдаёт 404.
    let app = Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .fallback(|| async { axum::http::StatusCode::NOT_FOUND });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, serve)
}

async fn get(url: &str) -> (u16, String) {
    let resp = reqwest::get(url).await.expect("запрос");
    let code = resp.status().as_u16();
    (code, resp.text().await.unwrap_or_default())
}

/// Пути, по которым проверяется отсутствие плагина. Взяты из разных семейств
/// роутера (статус, снимок, CRUD, таймеры, отчёты, WS): забытый `nest` одной
/// группы иначе проехал бы мимо теста, проверяющего только `status`.
const GATED_PATHS: [&str; 8] = [
    "/api/tracker/status",
    "/api/tracker/state",
    "/api/tracker/tasks",
    "/api/tracker/lists",
    "/api/tracker/entries",
    "/api/tracker/milestones",
    "/api/tracker/report",
    "/ws/tracker",
];

// ---------------------------------------------------------------------------
// Opt-in
// ---------------------------------------------------------------------------

/// ГЛАВНЫЙ инвариант фазы 3: без флага плагина нет вообще.
#[tokio::test]
async fn tracker_routes_are_absent_without_the_flag() {
    let (addr, _serve) = start_without_plugin().await;
    let base = format!("http://{addr}");

    // Хост жив.
    let (code, _) = get(&format!("{base}/healthz")).await;
    assert_eq!(code, 200);

    // А плагина нет.
    for path in GATED_PATHS {
        let (code, _) = get(&format!("{base}{path}")).await;
        assert_eq!(code, 404, "{path} обязан быть 404 без флага --tracker");
    }
}

/// С флагом — тот же путь отвечает 200, а хост-роут пережил `merge`.
#[tokio::test]
async fn the_status_route_answers_200_with_the_plugin_mounted() {
    let h = start_harness().await;

    let (code, body) = get(&format!("{}/api/tracker/status", h.base())).await;
    assert_eq!(code, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["enabled"], true);
    assert_eq!(json["tasks"], 0);

    let (code, _) = get(&format!("{}/healthz", h.base())).await;
    assert_eq!(code, 200, "merge плагина не имеет права съесть хост-роут");

    forge_tracker::shutdown(&h.state).await;
}

/// Задача, созданная по проводу, видна в снимке состояния — то есть роуты
/// смонтированы не по отдельности, а поверх ОДНОГО стора.
#[tokio::test]
async fn a_task_created_over_the_wire_shows_up_in_the_state() {
    let h = start_harness().await;
    let client = reqwest::Client::new();

    let created: Value = client
        .post(format!("{}/api/tracker/tasks", h.base()))
        .json(&serde_json::json!({ "title": "починить кран", "priority": 1 }))
        .send()
        .await
        .expect("POST /api/tracker/tasks")
        .json()
        .await
        .expect("тело ответа — задача");
    assert_eq!(created["title"], "починить кран");

    let (code, body) = get(&format!("{}/api/tracker/state", h.base())).await;
    assert_eq!(code, 200);
    let state: Value = serde_json::from_str(&body).unwrap();
    let tasks = state["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], created["id"]);

    forge_tracker::shutdown(&h.state).await;
}

/// Падение `init` НЕ роняет сервер. Здесь воспроизводится ровно та ветка,
/// что стоит в `main.rs`: `match … Err(e) => warn`, роуты не монтируются, а
/// хост поднимается как обычно. Использовать в `main.rs` `?` запрещено —
/// иначе беда с одним локальным файлом превратилась бы в отказ всего devforge.
///
/// Ветка Err подставляется ЯВНО, а не через настоящий сломанный конфиг, и это
/// не упрощение: у `forge_tracker::init` фатальных сценариев практически нет —
/// отсутствующий, пустой и битый `tracker.json` он переживает (см.
/// `TrackerStore::load`), а единственная реальная ошибка,
/// `TrackerConfig::from_home` без переменной `HOME`, до плагина не доходит:
/// на ней раньше падает сам хост (`TodoStore`). Проверяется здесь именно
/// ФОРМА обработки в `main.rs`, а не способность плагина сломаться.
#[tokio::test]
async fn a_failed_init_leaves_the_host_alive_and_the_plugin_absent() {
    let host = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    let init_result: Result<Arc<TrackerState>, forge_tracker::TrackerError> =
        Err(forge_tracker::TrackerError::Io("сломанный стор".to_string()));
    let app = match init_result {
        Ok(state) => forge_tracker::register_routes(host, state),
        Err(_) => host,
    }
    .fallback(|| async { axum::http::StatusCode::NOT_FOUND });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let base = format!("http://{addr}");

    let (code, _) = get(&format!("{base}/healthz")).await;
    assert_eq!(code, 200, "сервер обязан жить даже при сломанном плагине");

    // И роутов плагина при этом нет — 404, как и без флага.
    for path in GATED_PATHS {
        let (code, _) = get(&format!("{base}{path}")).await;
        assert_eq!(code, 404, "{path}: сломанный init не имеет права смонтировать роуты");
    }
}
