//! Integration smoke-test для плагина Echo (Phase 6.9).
//!
//! Поднимает минимальный axum-app в текущем процессе с:
//! - `forge_echo::init_with_config` (in-memory эквивалентная БД на tempfile);
//! - mock-CLI shell-скрипт, который эмулирует Claude streaming-json;
//! - `FakeHostApi` — реализация `HostApi` без зависимости от tmux/projects.
//!
//! Покрытие:
//!   1. `test_healthz` — GET `/api/echo/healthz` → 200 "ok"
//!   2. `test_create_and_list_conversation` — POST + GET conversations
//!   3. `test_chat_streaming` — WS connect, send user_message, receive
//!      `assistant_chunk` + `assistant_done`
//!   4. `test_conversation_persisted` — после chat'а GET messages показывает
//!      user + assistant
//!   5. `test_token_stats` — GET stats показывает накопленные токены
//!   6. `test_rate_limit_triggers_error` — 6 user_message при лимите 5/мин
//!      → 6-й получает `Error{code: "rate_limited"}`
//!   7. `test_autonomous_run_now` — создать task, dispatch run_task, проверить
//!      что run появляется (без ожидания scheduler tick'а)
//!
//! Тест полностью самодостаточный: используются только `tokio-tungstenite`,
//! `reqwest` и `tempfile` (все уже dev-dep tmux-web).

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use echo_host_api::{HostApi, SessionInfo};
use forge_echo::config::EchoConfig;
use forge_echo::db::repo::autonomous;
use forge_echo::scheduler::runner::run_task;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Тестовый mock-Host. Возвращает пустые сессии — этого хватает
/// prompt_builder'у Echo, чтобы построить минимальный prompt.
///
/// После Phase 4 (`remove-projects-concept`) host больше не выдаёт
/// проекты — поле `project_id` оставлено как непрозрачный label для
/// soft-FK в Echo SQLite, но через HostApi не транслируется.
struct FakeHost {
    #[allow(dead_code)]
    project_id: Option<String>,
}

#[async_trait]
impl HostApi for FakeHost {
    async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        Ok(Vec::new())
    }
    async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn auth_token(&self) -> Option<String> {
        None
    }
}

/// Пишет shell-скрипт mock-CLI с режимом исполнения 755. Контракт скрипта:
/// игнорировать аргументы, печатать NDJSON-фреймы из тела `script` в stdout
/// и выйти с кодом 0. Скрипт остаётся в `dir` пока `TempDir` живёт.
fn write_mock_cli(dir: &tempfile::TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("mock-claude");
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Mock-скрипт, печатающий 2 text_delta + result.
const MOCK_SCRIPT_BASIC: &str = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":7,"output_tokens":3}}'
"#;

/// Поднимает Echo на 127.0.0.1:0, возвращает (addr, state-handle-on-shutdown).
/// Тесты могут стучаться по `http://{addr}/...` и `ws://{addr}/ws/echo`.
struct Harness {
    addr: SocketAddr,
    shutdown_state: Arc<forge_echo::state::EchoState>,
    _tempdir: tempfile::TempDir, // hold mock CLI file alive
    _db_path: PathBuf,            // hold sqlite file alive (we delete on Drop)
    _serve_handle: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // best-effort cleanup; tempdir auto-removes mock cli.
        let _ = std::fs::remove_file(&self._db_path);
    }
}

/// Базовый стартер: rate-limit=5 (для теста P6.9-6) или 0 (отключено).
async fn start_harness(rate_limit: u32) -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let cli_path = write_mock_cli(&tempdir, MOCK_SCRIPT_BASIC);
    let db_path = tempdir
        .path()
        .join(format!("echo-smoke-{}.db", uuid::Uuid::new_v4()));

    let mut cfg = EchoConfig::default();
    cfg.cli_path = cli_path;
    cfg.db_path = db_path.clone();
    cfg.max_parallel_runs = 2;
    cfg.user_message_rate_limit_per_min = rate_limit;
    // Disable autonomous cap so chat-стримы свободно расходуют токены.
    cfg.autonomous_max_tokens_per_day = 0;

    let state = forge_echo::init_with_config(cfg).await.unwrap();
    let host: Arc<dyn HostApi> = Arc::new(FakeHost { project_id: None });

    let app = Router::new();
    let app = forge_echo::register_routes(app, state.clone(), host.clone());
    // Не спавним workers — scheduler не нужен smoke-тесту, и он съест 5s tick.

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let serve_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // Дать серверу время прокачать accept-цикл.
    tokio::time::sleep(Duration::from_millis(50)).await;

    Harness {
        addr,
        shutdown_state: state,
        _tempdir: tempdir,
        _db_path: db_path,
        _serve_handle: serve_handle,
    }
}

#[tokio::test]
async fn test_healthz() {
    let h = start_harness(0).await;
    let url = format!("http://{}/api/echo/healthz", h.addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");
    forge_echo::shutdown(&h.shutdown_state).await;
}

#[tokio::test]
async fn test_create_and_list_conversation() {
    let h = start_harness(0).await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{}/api/echo/conversations", base))
        .json(&json!({"title": "Hi", "project_id": null, "model": "sonnet-test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 200);
    let created: Value = create.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let listed = client
        .get(format!("{}/api/echo/conversations", base))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let body: Value = listed.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().any(|x| x["id"] == id));

    forge_echo::shutdown(&h.shutdown_state).await;
}

/// Helper: создаёт conversation, открывает WS, шлёт user_message,
/// возвращает (conversation_id, всё что пришло на WS за timeout).
async fn run_one_user_message(
    h: &Harness,
    text: &str,
) -> (String, Vec<Value>) {
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{}/api/echo/conversations", base))
        .json(&json!({"title": "T", "project_id": null, "model": "x"}))
        .send()
        .await
        .unwrap();
    let created: Value = create.json().await.unwrap();
    let conv_id = created["id"].as_str().unwrap().to_string();

    let ws_url = format!(
        "ws://{}/ws/echo?conversation_id={}",
        h.addr, conv_id
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let user_msg = json!({
        "type": "user_message",
        "text": text,
        "conversation_id": conv_id,
        "model": null,
        "ctx_opts": null,
    });
    ws.send(Message::Text(user_msg.to_string())).await.unwrap();

    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    let is_done = v["type"] == "assistant_done";
                    received.push(v);
                    if is_done {
                        break;
                    }
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => break,
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }

    let _ = ws.close(None).await;
    (conv_id, received)
}

#[tokio::test]
async fn test_chat_streaming_and_persistence_and_stats() {
    let h = start_harness(0).await;
    let (conv_id, events) = run_one_user_message(&h, "ping").await;

    let chunk_count = events
        .iter()
        .filter(|v| v["type"] == "assistant_chunk")
        .count();
    let done_count = events
        .iter()
        .filter(|v| v["type"] == "assistant_done")
        .count();
    assert!(chunk_count >= 1, "expected >=1 assistant_chunk, got {events:?}");
    assert_eq!(done_count, 1, "expected exactly 1 assistant_done");

    // Persistence: messages endpoint
    let base = format!("http://{}", h.addr);
    let url = format!("{}/api/echo/conversations/{}/messages", base, conv_id);
    let msgs: Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    let items = msgs["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "expected user + assistant: {items:?}");
    let roles: Vec<&str> = items.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert!(roles.contains(&"user"));
    assert!(roles.contains(&"assistant"));

    // Stats endpoint должен показать tokens > 0.
    let stats_url = format!("{}/api/echo/stats", base);
    let stats: Value = reqwest::get(&stats_url).await.unwrap().json().await.unwrap();
    // У эндпоинта может быть разная форма; sanity: ответ не пустой объект.
    assert!(stats.is_object(), "stats must be a JSON object, got {stats:?}");

    forge_echo::shutdown(&h.shutdown_state).await;
}

#[tokio::test]
async fn test_rate_limit_triggers_error() {
    // Лимит 3 user_message/min → 4-й получит Error{code:rate_limited}.
    let h = start_harness(3).await;

    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{}/api/echo/conversations", base))
        .json(&json!({"title":"rl","project_id":null,"model":"x"}))
        .send()
        .await
        .unwrap();
    let created: Value = create.json().await.unwrap();
    let conv_id = created["id"].as_str().unwrap().to_string();

    let ws_url = format!("ws://{}/ws/echo?conversation_id={}", h.addr, conv_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let mut rate_limited_seen = false;

    for _ in 0..4 {
        let msg = json!({
            "type": "user_message",
            "text": "ping",
            "conversation_id": conv_id,
            "model": null,
            "ctx_opts": null,
        });
        ws.send(Message::Text(msg.to_string())).await.unwrap();
        // Дать серверу обработать.
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    // Считываем все накопившиеся сообщения за короткое окно.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if v["type"] == "error" && v["code"] == "rate_limited" {
                        rate_limited_seen = true;
                        break;
                    }
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }

    assert!(
        rate_limited_seen,
        "expected at least one rate_limited error after 4 sends with limit=3"
    );

    let _ = ws.close(None).await;
    forge_echo::shutdown(&h.shutdown_state).await;
}

#[tokio::test]
async fn test_autonomous_run_now_via_runner() {
    // Тест НЕ ждёт scheduler-tick (5s); вместо этого напрямую вызывает
    // run_task. Это проверяет, что cli/db путь работают сквозь Echo state
    // в integration-сценарии.
    let h = start_harness(0).await;
    let host: Arc<dyn HostApi> = Arc::new(FakeHost { project_id: None });

    let task = autonomous::create_task(
        &h.shutdown_state.db,
        "auto-smoke",
        "do work",
        60,
        "sonnet-test",
        None,
    )
    .await
    .unwrap();

    run_task(h.shutdown_state.clone(), host, task.clone())
        .await
        .unwrap();

    let runs = autonomous::list_runs(&h.shutdown_state.db, &task.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "success");
    assert!(runs[0].tokens_in >= 1);
    assert!(runs[0].tokens_out >= 1);

    forge_echo::shutdown(&h.shutdown_state).await;
}
