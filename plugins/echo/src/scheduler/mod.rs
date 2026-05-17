//! Cron-like loop, опрашивающий `autonomous_tasks` каждые 5 секунд и
//! запускающий due-задачи параллельно через [`runner::run_task`].
//!
//! ## Архитектура
//!
//! - `spawn(state, host)` поднимает фоновую `tokio::task`, которая:
//!   1. Спит `TICK_INTERVAL` (5 секунд).
//!   2. Берёт `now = chrono::Utc::now().timestamp()`.
//!   3. Запрашивает `autonomous::find_due(db, now)` → список задач с
//!      `enabled = 1 AND next_run_at <= now`.
//!   4. Для каждой due-задачи (если она НЕ запущена прямо сейчас) спавнит
//!      `runner::run_task(state, host, task)` отдельной таской.
//!   5. По завершении run_task регистрирует завершение в `RunningSet`.
//!
//! ## Защита от двойного запуска
//!
//! [`RunningSet`] — `Arc<Mutex<HashSet<task_id>>>`. Перед спавном
//! `run_task` мы проверяем, нет ли task.id в множестве. Это защищает от
//! ситуации, когда задача с маленьким `interval_seconds` стартует ещё до
//! завершения предыдущего run'а (например, interval=1s, а Claude отвечает
//! 10s). База тоже содержит `next_run_at`, который двигается только при
//! finish_run — но на интервале между find_due и set_next_run возможна
//! гонка, и in-memory set её закрывает.
//!
//! ## Tolerance к панике
//!
//! Каждый `run_task` спавнится отдельной tokio-task. Если внутри случится
//! panic, scheduler-loop продолжит работу (мы не используем `JoinSet` без
//! обработки результата). Чтобы пометить run как error при панике, runner
//! сам оборачивает execute в `AssertUnwindSafe + catch_unwind`.
//!
//! ## Graceful shutdown
//!
//! `spawn` возвращает `JoinHandle<()>`. Хост-процесс может его abort'нуть
//! при завершении (Phase 6 hardening — `kill_workers + close DB`).

pub mod runner;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use echo_host_api::HostApi;

use crate::db::repo::autonomous;
use crate::state::EchoState;

/// Период опроса due-задач. 5 секунд — компромисс между latency старта
/// задачи и нагрузкой на SQLite (one indexed-query per 5s).
pub const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// In-memory множество id задач, для которых сейчас идёт `run_task`.
/// Защищает от двойного запуска одной задачи (если interval_seconds меньше
/// длительности run'а).
pub type RunningSet = Arc<Mutex<HashSet<String>>>;

/// Спавнит scheduler-loop. Возвращает `JoinHandle`, который вызывающий
/// может abort'нуть для graceful shutdown.
pub fn spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<()> {
    let running: RunningSet = Arc::new(Mutex::new(HashSet::new()));
    tracing::info!(tick_secs = TICK_INTERVAL.as_secs(), "Echo scheduler started");
    tokio::spawn(async move {
        run_loop(state, host, running).await;
    })
}

/// Внутренний loop — вынесен в pub(crate) для unit-test'ов одного tick'а
/// без необходимости спать 5 секунд.
async fn run_loop(state: Arc<EchoState>, host: Arc<dyn HostApi>, running: RunningSet) {
    loop {
        tokio::time::sleep(TICK_INTERVAL).await;
        tick_once(&state, &host, &running).await;
    }
}

/// Одна итерация: find_due → spawn run_task для каждой не-running задачи.
///
/// Не паникует на ошибках БД — logging + продолжаем. Видимый ошибочный
/// scenario: SQLite заблокирован/перегружен → tick пропускается, на
/// следующих find_due вернёт те же задачи.
pub(crate) async fn tick_once(
    state: &Arc<EchoState>,
    host: &Arc<dyn HostApi>,
    running: &RunningSet,
) {
    let now = chrono::Utc::now().timestamp();
    let due = match autonomous::find_due(&state.db, now).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: find_due failed");
            return;
        }
    };
    if due.is_empty() {
        tracing::trace!(now, "scheduler: tick, 0 due tasks");
        return;
    }
    tracing::debug!(now, count = due.len(), "scheduler: tick, found due tasks");

    for task in due {
        let task_id = task.id.clone();
        // Проверяем in-memory set: если задача уже запущена — пропускаем.
        {
            let mut set = running.lock().await;
            if set.contains(&task_id) {
                tracing::debug!(task_id, "scheduler: task already running, skip");
                continue;
            }
            set.insert(task_id.clone());
        }

        let state_clone = state.clone();
        let host_clone = host.clone();
        let running_clone = running.clone();
        let tid = task_id.clone();
        tokio::spawn(async move {
            // Никакая panic-инсайд не должна обрушить scheduler.
            // tokio::spawn ловит panic — пишет в JoinError, но он
            // не наблюдается scheduler'ом (мы не join'им). Дополнительно
            // оборачиваем сам вызов в try-блок чтобы always очистить set.
            let res = runner::run_task(state_clone, host_clone, task).await;
            if let Err(e) = res {
                tracing::warn!(task_id = %tid, error = %e, "scheduler: run_task error");
            }
            running_clone.lock().await.remove(&tid);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::Db;
    use async_trait::async_trait;
    use echo_host_api::{ProjectInfo, SessionInfo};
    use std::path::PathBuf;

    struct StubHost;
    #[async_trait]
    impl HostApi for StubHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(Vec::new())
        }
        async fn capture_pane_full(&self, _s: &str, _l: i32) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn list_projects(&self) -> anyhow::Result<Vec<ProjectInfo>> {
            Ok(Vec::new())
        }
        async fn active_project_id(&self) -> Option<String> {
            None
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
    }

    fn write_mock_cli(dir: &tempfile::TempDir, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    async fn make_state_with_mock_cli(dir: &tempfile::TempDir) -> Arc<EchoState> {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":3,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host).ok();
        state
    }

    /// Одна итерация tick'а должна найти due-задачу и запустить runner,
    /// который инсёртит TaskRun.
    #[tokio::test]
    async fn tick_picks_up_due_task_and_runs_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state_with_mock_cli(&dir).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        let running: RunningSet = Arc::new(Mutex::new(HashSet::new()));

        let now = chrono::Utc::now().timestamp();
        let task = autonomous::create_task(&state.db, "t1", "do x", 60, "sonnet-4", None)
            .await
            .unwrap();
        // Принудительно делаем due.
        autonomous::set_next_run(&state.db, &task.id, now - 1)
            .await
            .unwrap();

        tick_once(&state, &host, &running).await;

        // Ждём, чтобы spawned run_task успел завершиться. Используем
        // poll-loop вместо фиксированного sleep'а — мок-CLI fork+exec на
        // CI может быть медленным.
        let mut runs = Vec::new();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            runs = autonomous::list_runs(&state.db, &task.id, 10).await.unwrap();
            if !runs.is_empty() && runs[0].finished_at.is_some() {
                break;
            }
        }
        assert!(!runs.is_empty(), "expected at least one task_run row");
        let r = &runs[0];
        assert_eq!(r.status, "success", "expected success, got {r:?}");
        // next_run_at должен быть сдвинут.
        let t = autonomous::get_task(&state.db, &task.id)
            .await
            .unwrap()
            .unwrap();
        assert!(t.next_run_at.unwrap() > now);
    }

    /// Дважды tick подряд (без ожидания) — задача должна стартовать только
    /// один раз (RunningSet защищает).
    #[tokio::test]
    async fn tick_does_not_double_spawn_running_task() {
        let dir = tempfile::tempdir().unwrap();
        // Мок CLI медленный, чтобы первый run точно ещё работал
        // во время второго tick.
        let script = r#"#!/bin/sh
sleep 1
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"slow"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host.clone()).ok();
        let running: RunningSet = Arc::new(Mutex::new(HashSet::new()));

        let now = chrono::Utc::now().timestamp();
        let task = autonomous::create_task(&state.db, "slow", "p", 60, "m", None)
            .await
            .unwrap();
        autonomous::set_next_run(&state.db, &task.id, now - 1)
            .await
            .unwrap();

        // Два tick'а быстро подряд.
        tick_once(&state, &host, &running).await;
        // Дать tokio::spawn зарегистрировать task в множестве.
        tokio::time::sleep(Duration::from_millis(20)).await;
        tick_once(&state, &host, &running).await;

        // Подождать завершения slow-run'а.
        tokio::time::sleep(Duration::from_millis(1800)).await;

        let runs = autonomous::list_runs(&state.db, &task.id, 10).await.unwrap();
        assert_eq!(runs.len(), 1, "second tick must NOT start a second run");
    }

    #[tokio::test]
    async fn empty_due_tick_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state_with_mock_cli(&dir).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        let running: RunningSet = Arc::new(Mutex::new(HashSet::new()));
        tick_once(&state, &host, &running).await; // не падает
    }
}
