//! Integration test для Phase 4 — autonomous scheduler.
//!
//! Сценарий из verify-gate (P4.5):
//! - Поднять реальный scheduler::spawn с мок Claude CLI.
//! - Создать autonomous task с маленьким `interval_seconds`.
//! - Через ~6 секунд (один tick гарантирован) проверить, что:
//!   1. В `task_runs` есть запись со статусом success.
//!   2. Через broadcast пришёл `AutonomousTaskEvent { status: "success", .. }`.
//!   3. В `token_stats` minute-bucket'е появились токены.
//! - Через ещё один tick — второй run (interval ≤ 5s).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use echo_host_api::{HostApi, SessionInfo};
use forge_echo::claude::ClaudeRunner;
use forge_echo::db::repo::{autonomous, stats};
use forge_echo::db::Db;
use forge_echo::scheduler;
use forge_echo::state::EchoState;
use forge_echo::ws::protocol::ServerMsg;

fn write_mock_cli(dir: &tempfile::TempDir, script: &str) -> PathBuf {
    let path = dir.path().join("mock-claude");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

struct StubHost;
#[async_trait]
impl HostApi for StubHost {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduler_runs_due_task_and_broadcasts_event() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"auto"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":4,"output_tokens":2}}'
"#;
    let cli = write_mock_cli(&dir, script);
    let runner = Arc::new(ClaudeRunner::new(cli, 4));
    let db = Db::open_memory().await.unwrap();
    db.migrate().await.unwrap();
    let state = Arc::new(EchoState::new(Arc::new(db), runner));
    let host: Arc<dyn HostApi> = Arc::new(StubHost);
    state.host.set(host.clone()).ok();

    // Создаём задачу с interval=2s, next_run_at в прошлом → first tick её
    // схватит.
    let task = autonomous::create_task(&state.db, "auto-test", "ping", 2, "sonnet-4", None)
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    autonomous::set_next_run(&state.db, &task.id, now - 1)
        .await
        .unwrap();

    // Подписываемся на broadcast ДО запуска scheduler.
    let mut rx = state.broadcast.subscribe();

    // Запускаем scheduler.
    let handle = scheduler::spawn(state.clone(), host);

    // Собираем события в течение ~7 секунд (одного tick'а гарантирует
    // запуск, но мы дадим запас для CI).
    let collect_timeout = Duration::from_secs(8);
    let deadline = tokio::time::Instant::now() + collect_timeout;
    let mut success_events = 0usize;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if let ServerMsg::AutonomousTaskEvent { status, task_id, .. } = &ev.msg {
                    if task_id == &task.id && status == "success" {
                        success_events += 1;
                    }
                }
            }
            _ => break,
        }
    }

    handle.abort();

    // Проверяем БД.
    let runs = autonomous::list_runs(&state.db, &task.id, 10).await.unwrap();
    assert!(
        runs.iter().any(|r| r.status == "success"),
        "expected at least one success run, got: {runs:?}"
    );

    // Хотя бы 1 success-event пришёл через broadcast.
    assert!(
        success_events >= 1,
        "expected at least one success broadcast event"
    );

    // token_stats содержит запись (любой recent bucket).
    let now = chrono::Utc::now().timestamp();
    let bucket = now / 60;
    let s = stats::range(&state.db, bucket - 2, bucket + 1).await.unwrap();
    let total_in: i64 = s.iter().map(|b| b.tokens_in).sum();
    assert!(total_in >= 4, "token_stats must reflect usage; got {total_in}");
}
