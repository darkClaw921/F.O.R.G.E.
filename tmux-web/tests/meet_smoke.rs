//! Смоук-тесты плагина встреч ПО ПРОВОДУ (близнец `bitrix_smoke.rs`).
//!
//! Отличие от юнит-тестов внутри `plugins/meet/src/**`: здесь поднимается
//! НАСТОЯЩИЙ axum-сервер на свободном порту и запросы идут через реальный
//! HTTP-клиент. Это ловит класс дефектов, который `oneshot` пропускает:
//! несмонтированный роут, конфликт шаблонов путей, сломанный WS-апгрейд,
//! порядок слоёв.
//!
//! # Что покрыто
//!
//! **Opt-in — главный инвариант фичи.** [`meet_routes_are_absent_without_the_flag`]
//! собирает роутер БЕЗ `register_routes` и убеждается, что весь префикс
//! `/api/meet/*` и `/ws/meet` отвечают 404. Ровно на этом 404 держится
//! фронтовый гейт: `meetEnabled()` навсегда `false`, `attachMeetButton`
//! выходит первой строкой, батчей нет, WS не открывается. Подмонтируй
//! кто-нибудь `/api/meet/status` мимо флага — кнопка записи появилась бы у
//! всех, кто запускает devforge без `--meet`.
//!
//! **`status` не падает** ни при отсутствующих бинарях, ни без базы.
//!
//! **Падение `init` не роняет сервер**: воспроизводится тот же `match … Err
//! => warn`, что стоит в `main.rs`, и проверяется, что соседние роуты живы.
//!
//! **WS**: апгрейд, первый кадр `snapshot`, кадр из broadcast доезжает,
//! битый входящий кадр НЕ рвёт соединение.
//!
//! Тесты не требуют ни сети, ни Chrome, ни ffmpeg, ни whisper. Все пути —
//! во временном каталоге, поэтому живые `~/.config/forge/meet.toml` и
//! `~/.forge/meet` разработчика не читаются и не перезаписываются.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use forge_meet::config::MeetConfig;
use forge_meet::state::MeetState;
use forge_meet::ws::protocol::MeetServerMsg;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

/// Живой сервер с примонтированным плагином.
struct Harness {
    addr: SocketAddr,
    state: Arc<MeetState>,
    _tempdir: tempfile::TempDir,
    _serve: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// ГЕРМЕТИЧНЫЙ конфиг: всё во временном каталоге.
///
/// Без этого прогон тестов открыл бы живую базу встреч пользователя и
/// прошёлся бы `reconcile_on_boot` по его записям — то есть перевёл бы
/// настоящие, идущие прямо сейчас встречи в `failed`.
fn test_config(dir: &std::path::Path) -> MeetConfig {
    MeetConfig {
        config_path: dir.join("meet.toml"),
        cache_dir: dir.join("cache"),
        db_path: dir.join("cache/meet.db"),
        models_dir: dir.join("cache/models"),
        claude_cli_path: dir.join("no-such-claude"),
        ..MeetConfig::default()
    }
}

/// Поднимает сервер С плагином.
async fn start_harness(ws: Option<(Duration, Duration)>) -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let cfg = test_config(tempdir.path());

    let state = match ws {
        Some((hb, idle)) => MeetState::with_ws_timings(cfg, hb, idle),
        None => forge_meet::init(cfg).expect("init обязан отработать на валидном конфиге"),
    };
    forge_meet::spawn_workers(&state).await;

    // Хост-роут рядом: он обязан пережить merge плагина.
    let host = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    let app: Router = forge_meet::register_routes(host, state.clone());

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
/// `--meet`.
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

// ---------------------------------------------------------------------------
// Opt-in
// ---------------------------------------------------------------------------

/// ГЛАВНЫЙ инвариант фичи: без флага плагина нет вообще.
#[tokio::test]
async fn meet_routes_are_absent_without_the_flag() {
    let (addr, _serve) = start_without_plugin().await;
    let base = format!("http://{addr}");

    // Хост жив.
    let (code, _) = get(&format!("{base}/healthz")).await;
    assert_eq!(code, 200);

    // А плагина нет. 404 на `/api/meet/status` — это КОНТРАКТ фронтового
    // гейта, а не «просто ошибка»: по нему фронт понимает, что кнопку встречи
    // рисовать не нужно, и делает ровно один GET за загрузку страницы.
    for path in [
        "/api/meet/status",
        "/api/meet/badges?plugin=telegram&chat_id=7&min_id=1&max_id=9",
        "/api/meet/meetings",
        "/ws/meet",
    ] {
        let (code, _) = get(&format!("{base}{path}")).await;
        assert_eq!(code, 404, "{path} обязан быть 404 без флага --meet");
    }
}

/// С флагом — тот же путь отвечает 200.
#[tokio::test]
async fn the_status_route_answers_200_with_the_plugin_mounted() {
    let h = start_harness(None).await;
    let (code, body) = get(&format!("{}/api/meet/status", h.base())).await;
    assert_eq!(code, 200);

    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["enabled"], true);
    assert_eq!(json["limits"]["whisper"], 1, "лимит whisper поднять нельзя");
    assert_eq!(json["providers"].as_array().unwrap().len(), 4);
    // Реестр приезжает с бэкенда вместе с регулярками: разметку браузер
    // держит в кэше PWA неделями.
    assert!(json["providers"][0]["link_regex"].as_str().unwrap().contains("telemost"));

    // И хост-роут пережил merge.
    let (code, _) = get(&format!("{}/healthz", h.base())).await;
    assert_eq!(code, 200);

    forge_meet::shutdown(&h.state).await;
}

/// Плагин обязан отвечать 200 при полностью пустом окружении: ни одного
/// внешнего бинаря нет, и именно этим ответом пользователь узнаёт, чего не
/// хватает.
#[tokio::test]
async fn the_status_route_never_fails_without_external_binaries() {
    let h = start_harness(None).await;
    let (code, body) = get(&format!("{}/api/meet/status", h.base())).await;
    assert_eq!(code, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["deps"]["probed"], false, "пробы бинарей — работа Ф5");
    assert_eq!(json["deps"]["ffmpeg"]["found"], Value::Null, "«не смотрели» ≠ «не нашли»");
    assert_eq!(json["db_ready"], true, "spawn_workers обязан был открыть базу");
    forge_meet::shutdown(&h.state).await;
}

/// Падение `init` не имеет права уронить сервер: в `main.rs` стоит
/// `match … Err => warn`, а не `?`. Тест воспроизводит ту же развилку.
#[tokio::test]
async fn a_failing_init_leaves_the_rest_of_the_server_alive() {
    let failed: anyhow::Result<Arc<MeetState>> = Err(anyhow::anyhow!("битый meet.toml"));

    let host = Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .fallback(|| async { axum::http::StatusCode::NOT_FOUND });
    // Ровно та же конструкция, что в main.rs.
    let app = match failed {
        Ok(state) => forge_meet::register_routes(host, state),
        Err(_e) => host,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (code, _) = get(&format!("http://{addr}/healthz")).await;
    assert_eq!(code, 200, "сервер обязан жить без плагина");
    let (code, _) = get(&format!("http://{addr}/api/meet/status")).await;
    assert_eq!(code, 404, "плагина нет — значит 404, а не 500");
}

// ---------------------------------------------------------------------------
// Батч ленты
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_badges_route_answers_over_the_wire() {
    let h = start_harness(None).await;
    let (code, body) = get(&format!(
        "{}/api/meet/badges?plugin=telegram&chat_id=7&min_id=1&max_id=50",
        h.base()
    ))
    .await;
    assert_eq!(code, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    // Встреч нет — значит и бейджей нет. Отсутствие бейджа и означает
    // «можно записать».
    assert_eq!(json["badges"].as_array().unwrap().len(), 0);

    // Битые параметры — 400 с машиночитаемым кодом.
    let (code, body) = get(&format!("{}/api/meet/badges?plugin=telegram", h.base())).await;
    assert_eq!(code, 400);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"], "bad_request");

    forge_meet::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_websocket_sends_a_snapshot_first_and_survives_junk() {
    // Тайминги укорочены: иначе тест ждал бы боевую минуту.
    let h = start_harness(Some((Duration::from_millis(120), Duration::from_secs(30)))).await;
    let url = format!("ws://{}/ws/meet?token=", h.addr);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("апгрейд /ws/meet");

    // Первый кадр — снапшот.
    let first = ws.next().await.expect("кадр").expect("не ошибка");
    let text = first.into_text().unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["type"], "snapshot", "{text}");
    assert_eq!(json["limits"]["whisper"], 1);

    // Битый входящий кадр НЕ рвёт соединение: незнакомое сообщение может
    // прийти от более новой версии фронта.
    ws.send(Message::Text("{\"type\":\"нет-такого\"}".into()))
        .await
        .unwrap();

    // Кадр из broadcast доезжает — фильтрации по встрече нет, поэтому
    // событие «чужой» встречи обязано дойти до любой вкладки.
    h.state.broadcast_event(MeetServerMsg::NeedsHuman {
        meeting_id: "чужая-встреча".into(),
        kind: "captcha".into(),
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_needs_human = false;
    while std::time::Instant::now() < deadline {
        let Ok(Some(Ok(msg))) =
            tokio::time::timeout(Duration::from_secs(1), ws.next()).await
        else {
            continue;
        };
        let Ok(text) = msg.into_text() else { continue };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if json["type"] == "needs_human" {
            assert_eq!(json["meeting_id"], "чужая-встреча");
            saw_needs_human = true;
            break;
        }
    }
    assert!(saw_needs_human, "кадр из broadcast не доехал");

    forge_meet::shutdown(&h.state).await;
}
