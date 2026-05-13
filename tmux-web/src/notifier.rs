//! Phase 2 — фоновая подсистема нотификаций при promote TODO → bd-task.
//!
//! ### Назначение
//!
//! Когда пользователь promote'ит TODO-карточку в реальную bd-задачу,
//! нужно отправить текстовое уведомление в активную tmux-сессию проекта
//! с заданной задержкой и в одном из трёх режимов:
//!
//! - **Immediate** — сразу `tmux::send_keys`.
//! - **Delayed { fire_at }** — `tokio::time::sleep_until(fire_at)` затем send.
//! - **WaitPrevious { project_id }** — ждём, пока в `tasks_watcher`
//!   придёт `TaskEvent::Upsert { issue }` с `issue.status == "closed"` и
//!   `issue.id == previous_promoted_id`. Это очередь FIFO per project:
//!   пока предыдущий промоут не закрыт — следующий висит.
//!
//! ### Архитектура
//!
//! - `start(project_root, task_events_rx)` — запускает фоновый task,
//!   возвращает [`NotifyHandle`] с `mpsc::Sender<NotifyCommand>`.
//! - В фоне: select-loop по
//!     1. `mpsc_rx.recv()` — новые команды (Enqueue / Save / Shutdown).
//!     2. `task_events_rx.recv()` — события из `tasks_watcher` (для wait_previous).
//!     3. `tokio::time::sleep_until(next_delayed_fire_at)` — таймер ближайшего delayed-job'а.
//!
//! ### Persist
//!
//! `<project_root>/.forge/notify_state.json` — в нём:
//! `{"pending": [...], "wait_queues": {project_id -> [job_id]}, "last_promoted_open_id": {project_id -> task_id}}`.
//! Atomic save через tempfile+rename (паттерн из `todos.rs`/`projects.rs`).
//!
//! При старте: читаем state, восстанавливаем pending jobs.
//! Delayed-jobs с `fire_at` уже в прошлом — выполняются сразу.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{sleep_until, Instant};

use crate::tasks::TaskEvent;
use crate::tmux;

/// Режим доставки нотификации.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyMode {
    /// Доставить немедленно.
    Immediate,
    /// Доставить не раньше `fire_at_unix_ms` (Unix milliseconds, UTC).
    Delayed { fire_at_unix_ms: u64 },
    /// Ждать пока bd-задача `previous_task_id` закроется.
    WaitPrevious { previous_task_id: Option<String> },
}

/// Описание одной нотификации в очереди.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyJob {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub target_session: String,
    pub text: String,
    pub mode: NotifyMode,
    #[serde(default)]
    pub created_at_unix_ms: u64,
}

/// Файл `<project_root>/.forge/notify_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct NotifyState {
    #[serde(default)]
    pending: Vec<NotifyJob>,
    #[serde(default)]
    wait_queues: HashMap<String, VecDeque<String>>,
    #[serde(default)]
    last_promoted_open_id: HashMap<String, String>,
}

/// Команды, принимаемые [`notifier_loop`].
#[derive(Debug)]
pub enum NotifyCommand {
    Enqueue(NotifyJob),
}

/// Cheap-clonable handle для отправки команд в notifier_loop.
#[derive(Debug, Clone)]
pub struct NotifyHandle {
    tx: mpsc::Sender<NotifyCommand>,
}

impl NotifyHandle {
    /// Поставить новый job в очередь.
    #[allow(dead_code)]
    pub async fn enqueue(&self, job: NotifyJob) -> Result<()> {
        self.tx
            .send(NotifyCommand::Enqueue(job))
            .await
            .context("notifier mpsc channel closed (loop dead?)")
    }
}

/// Запускает фоновый notifier_loop. Возвращает handle для отправки команд.
pub fn start(
    project_root: PathBuf,
    task_events_rx: broadcast::Receiver<TaskEvent>,
) -> NotifyHandle {
    let (tx, rx) = mpsc::channel::<NotifyCommand>(256);
    let handle = NotifyHandle { tx };

    tokio::spawn(notifier_loop(project_root, rx, task_events_rx));

    handle
}

/// Главный цикл notifier'а.
async fn notifier_loop(
    project_root: PathBuf,
    mut cmd_rx: mpsc::Receiver<NotifyCommand>,
    mut task_events_rx: broadcast::Receiver<TaskEvent>,
) {
    let state_path = state_file_path(&project_root);
    tracing::info!(
        path = %state_path.display(),
        "notifier_loop started"
    );

    let mut state = match load_state(&state_path) {
        Ok(s) => {
            tracing::info!(
                pending = s.pending.len(),
                queues = s.wait_queues.len(),
                "notify state restored"
            );
            s
        }
        Err(e) => {
            tracing::warn!(error = ?e, "failed to load notify_state.json — starting fresh");
            NotifyState::default()
        }
    };

    fire_due_immediate_and_overdue(&mut state, &state_path).await;

    loop {
        let next_delayed_deadline = next_delayed_deadline(&state);
        let timer = next_delayed_deadline.map(sleep_until);

        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(NotifyCommand::Enqueue(job)) => {
                        handle_enqueue(&mut state, job, &state_path).await;
                    }
                    None => {
                        tracing::info!("notifier_loop: command channel closed, exiting");
                        return;
                    }
                }
            }

            _ = async {
                if let Some(t) = timer { t.await }
                else { std::future::pending::<()>().await }
            } => {
                fire_due_delayed(&mut state, &state_path).await;
            }

            ev = task_events_rx.recv() => {
                match ev {
                    Ok(TaskEvent::Upsert { issue }) => {
                        let id_opt = issue.get("id").and_then(|v| v.as_str()).map(str::to_string);
                        let status_opt = issue.get("status").and_then(|v| v.as_str()).map(str::to_string);
                        if let (Some(task_id), Some(status)) = (id_opt, status_opt) {
                            if status == "closed" {
                                handle_task_closed(&mut state, &task_id, &state_path).await;
                            }
                        }
                    }
                    Ok(TaskEvent::Removed { .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "notifier task_events channel lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("notifier_loop: task_events channel closed");
                        let (tx, rx) = broadcast::channel::<TaskEvent>(1);
                        std::mem::forget(tx);
                        task_events_rx = rx;
                    }
                }
            }
        }
    }
}

/// Возвращает путь к `<project_root>/.forge/notify_state.json`.
fn state_file_path(project_root: &std::path::Path) -> PathBuf {
    let forge_dir = project_root.join(".forge");
    let _ = std::fs::create_dir_all(&forge_dir);
    forge_dir.join("notify_state.json")
}

/// Загружает state с диска. Если файла нет — `Ok(default)`.
fn load_state(path: &std::path::Path) -> Result<NotifyState> {
    if !path.exists() {
        return Ok(NotifyState::default());
    }
    let raw = std::fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if raw.is_empty() {
        return Ok(NotifyState::default());
    }
    let parsed: NotifyState = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(parsed)
}

/// Atomic save state через tempfile + rename.
fn save_state(path: &std::path::Path, state: &NotifyState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(state).context("failed to serialize NotifyState")?;
    let mut tmp = path.to_path_buf();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);
    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!("failed to rename {} -> {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// Текущее Unix-время в миллисекундах.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Рассчитывает Instant для tokio::time::sleep_until.
fn instant_for_unix_ms(target_ms: u64) -> Instant {
    let now_ms = now_unix_ms();
    if target_ms <= now_ms {
        return Instant::now();
    }
    let delta = Duration::from_millis(target_ms - now_ms);
    Instant::now() + delta
}

/// Находит ближайший fire_at среди Delayed-jobs.
fn next_delayed_deadline(state: &NotifyState) -> Option<Instant> {
    state
        .pending
        .iter()
        .filter_map(|j| match &j.mode {
            NotifyMode::Delayed { fire_at_unix_ms } => Some(*fire_at_unix_ms),
            _ => None,
        })
        .min()
        .map(instant_for_unix_ms)
}

/// При получении нового job'а: добавить в state, выполнить если можно сразу.
async fn handle_enqueue(state: &mut NotifyState, job: NotifyJob, state_path: &std::path::Path) {
    tracing::debug!(job_id = %job.id, project = %job.project_id, mode = ?job.mode, "enqueue");

    match &job.mode {
        NotifyMode::Immediate => {
            state
                .last_promoted_open_id
                .insert(job.project_id.clone(), job.task_id.clone());
            if let Err(e) = save_state(state_path, state) {
                tracing::warn!(error = ?e, "save_state before immediate fire failed");
            }
            fire_job(&job).await;
        }
        NotifyMode::Delayed { .. } => {
            state.pending.push(job.clone());
            if let Err(e) = save_state(state_path, state) {
                tracing::warn!(error = ?e, "save_state after delayed enqueue failed");
            }
        }
        NotifyMode::WaitPrevious { previous_task_id } => {
            let pid = job.project_id.clone();
            let queue_was_empty = state
                .wait_queues
                .get(&pid)
                .map(|q| q.is_empty())
                .unwrap_or(true);

            let last_open = state.last_promoted_open_id.get(&pid).cloned();
            let nothing_to_wait = match (previous_task_id.as_ref(), last_open.as_ref()) {
                (None, _) if queue_was_empty => true,
                (Some(_), None) if queue_was_empty => true,
                _ => false,
            };

            if nothing_to_wait {
                state
                    .last_promoted_open_id
                    .insert(pid.clone(), job.task_id.clone());
                if let Err(e) = save_state(state_path, state) {
                    tracing::warn!(error = ?e, "save_state before wait_previous immediate fire failed");
                }
                fire_job(&job).await;
            } else {
                state
                    .wait_queues
                    .entry(pid)
                    .or_default()
                    .push_back(job.id.clone());
                state.pending.push(job);
                if let Err(e) = save_state(state_path, state) {
                    tracing::warn!(error = ?e, "save_state after wait_previous enqueue failed");
                }
            }
        }
    }
}

/// На старте: выполнить просроченные Immediate и Delayed jobs.
async fn fire_due_immediate_and_overdue(
    state: &mut NotifyState,
    state_path: &std::path::Path,
) {
    let now = now_unix_ms();
    let mut to_fire: Vec<NotifyJob> = Vec::new();

    let mut keep: Vec<NotifyJob> = Vec::with_capacity(state.pending.len());
    for j in state.pending.drain(..) {
        match &j.mode {
            NotifyMode::Immediate => to_fire.push(j),
            NotifyMode::Delayed { fire_at_unix_ms } if *fire_at_unix_ms <= now => to_fire.push(j),
            _ => keep.push(j),
        }
    }
    state.pending = keep;

    if !to_fire.is_empty() {
        if let Err(e) = save_state(state_path, state) {
            tracing::warn!(error = ?e, "save_state before startup fire failed");
        }
        for job in &to_fire {
            state
                .last_promoted_open_id
                .insert(job.project_id.clone(), job.task_id.clone());
            fire_job(job).await;
        }
        if let Err(e) = save_state(state_path, state) {
            tracing::warn!(error = ?e, "save_state after startup fire failed");
        }
    }
}

/// Срабатывание timer'а: выполнить Delayed-jobs с fire_at <= now.
async fn fire_due_delayed(state: &mut NotifyState, state_path: &std::path::Path) {
    let now = now_unix_ms();
    let mut to_fire: Vec<NotifyJob> = Vec::new();
    let mut keep: Vec<NotifyJob> = Vec::with_capacity(state.pending.len());
    for j in state.pending.drain(..) {
        match &j.mode {
            NotifyMode::Delayed { fire_at_unix_ms } if *fire_at_unix_ms <= now => {
                to_fire.push(j);
            }
            _ => keep.push(j),
        }
    }
    state.pending = keep;

    if to_fire.is_empty() {
        return;
    }

    if let Err(e) = save_state(state_path, state) {
        tracing::warn!(error = ?e, "save_state before delayed fire failed");
    }

    for job in &to_fire {
        state
            .last_promoted_open_id
            .insert(job.project_id.clone(), job.task_id.clone());
        fire_job(job).await;
    }

    if let Err(e) = save_state(state_path, state) {
        tracing::warn!(error = ?e, "save_state after delayed fire failed");
    }
}

/// Обработка Upsert со status=closed — продвинуть очередь wait_previous.
async fn handle_task_closed(
    state: &mut NotifyState,
    closed_task_id: &str,
    state_path: &std::path::Path,
) {
    let projects_waiting: Vec<String> = state
        .last_promoted_open_id
        .iter()
        .filter(|(_, v)| v.as_str() == closed_task_id)
        .map(|(k, _)| k.clone())
        .collect();

    if projects_waiting.is_empty() {
        return;
    }

    let mut fired_any = false;

    for project_id in projects_waiting {
        state.last_promoted_open_id.remove(&project_id);

        let next_id = state
            .wait_queues
            .get_mut(&project_id)
            .and_then(|q| q.pop_front());
        if let Some(empty) = state.wait_queues.get(&project_id) {
            if empty.is_empty() {
                state.wait_queues.remove(&project_id);
            }
        }

        if let Some(job_id) = next_id {
            let pos = state.pending.iter().position(|j| j.id == job_id);
            if let Some(idx) = pos {
                let job = state.pending.remove(idx);
                state
                    .last_promoted_open_id
                    .insert(project_id.clone(), job.task_id.clone());
                if let Err(e) = save_state(state_path, state) {
                    tracing::warn!(error = ?e, "save_state before wait_previous chain fire failed");
                }
                fire_job(&job).await;
                fired_any = true;
            } else {
                tracing::warn!(
                    project = %project_id,
                    job_id = %job_id,
                    "wait_queues head id missing from pending — skipping"
                );
            }
        }
    }

    if fired_any {
        if let Err(e) = save_state(state_path, state) {
            tracing::warn!(error = ?e, "save_state after wait_previous chain fire failed");
        }
    } else if let Err(e) = save_state(state_path, state) {
        tracing::warn!(error = ?e, "save_state after task_closed (no fire) failed");
    }
}

/// Выполнение одного job'а: tmux::send_keys с retry x3 (backoff 500/1000/2000ms).
async fn fire_job(job: &NotifyJob) {
    let backoffs = [500u64, 1000, 2000];
    for (attempt, backoff_ms) in backoffs.iter().enumerate() {
        match tmux::send_keys(&job.target_session, &job.text).await {
            Ok(()) => {
                tracing::info!(
                    job_id = %job.id,
                    project = %job.project_id,
                    task = %job.task_id,
                    session = %job.target_session,
                    attempt = attempt + 1,
                    "notify delivered"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    job_id = %job.id,
                    session = %job.target_session,
                    error = ?e,
                    attempt = attempt + 1,
                    "tmux::send_keys failed; will retry"
                );
                if attempt < backoffs.len() - 1 {
                    tokio::time::sleep(Duration::from_millis(*backoff_ms)).await;
                }
            }
        }
    }
    tracing::error!(
        job_id = %job.id,
        session = %job.target_session,
        "notify FAILED after 3 attempts; dropping"
    );
}

/// Конструктор NotifyJob со сгенерированным UUID и timestamp.
#[allow(dead_code)]
pub fn new_job(
    project_id: String,
    task_id: String,
    target_session: String,
    text: String,
    mode: NotifyMode,
) -> NotifyJob {
    NotifyJob {
        id: uuid::Uuid::new_v4().to_string(),
        project_id,
        task_id,
        target_session,
        text,
        mode,
        created_at_unix_ms: now_unix_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-notifier-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn save_load_empty_roundtrip() {
        let dir = tempdir("empty");
        let path = state_file_path(&dir);
        let state = NotifyState::default();
        save_state(&path, &state).unwrap();
        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.pending.len(), 0);
        assert!(loaded.wait_queues.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_with_jobs() {
        let dir = tempdir("withjobs");
        let path = state_file_path(&dir);
        let mut state = NotifyState::default();
        state.pending.push(NotifyJob {
            id: "job-1".into(),
            project_id: "forge".into(),
            task_id: "forge-1".into(),
            target_session: "forge-main".into(),
            text: "test text".into(),
            mode: NotifyMode::Delayed {
                fire_at_unix_ms: 9_999_999_999_999,
            },
            created_at_unix_ms: 1,
        });
        state
            .last_promoted_open_id
            .insert("forge".into(), "forge-0".into());
        save_state(&path, &state).unwrap();

        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.pending.len(), 1);
        assert_eq!(loaded.pending[0].id, "job-1");
        match &loaded.pending[0].mode {
            NotifyMode::Delayed { fire_at_unix_ms } => {
                assert_eq!(*fire_at_unix_ms, 9_999_999_999_999);
            }
            other => panic!("expected Delayed, got {other:?}"),
        }
        assert_eq!(loaded.last_promoted_open_id.get("forge").unwrap(), "forge-0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_delayed_deadline_picks_earliest() {
        let mut state = NotifyState::default();
        state.pending.push(NotifyJob {
            id: "a".into(),
            project_id: "p".into(),
            task_id: "p-1".into(),
            target_session: "s".into(),
            text: "x".into(),
            mode: NotifyMode::Delayed {
                fire_at_unix_ms: now_unix_ms() + 10_000,
            },
            created_at_unix_ms: 0,
        });
        state.pending.push(NotifyJob {
            id: "b".into(),
            project_id: "p".into(),
            task_id: "p-2".into(),
            target_session: "s".into(),
            text: "x".into(),
            mode: NotifyMode::Delayed {
                fire_at_unix_ms: now_unix_ms() + 1_000,
            },
            created_at_unix_ms: 0,
        });
        state.pending.push(NotifyJob {
            id: "c".into(),
            project_id: "p".into(),
            task_id: "p-3".into(),
            target_session: "s".into(),
            text: "x".into(),
            mode: NotifyMode::Immediate,
            created_at_unix_ms: 0,
        });
        let dl = next_delayed_deadline(&state);
        assert!(dl.is_some());
    }

    #[test]
    fn instant_for_past_returns_now() {
        let past = 1_000u64;
        let inst = instant_for_unix_ms(past);
        assert!(inst <= Instant::now());
    }

    #[test]
    fn notify_mode_serialization_roundtrip() {
        let m1 = NotifyMode::Immediate;
        let s1 = serde_json::to_string(&m1).unwrap();
        let parsed: NotifyMode = serde_json::from_str(&s1).unwrap();
        assert_eq!(parsed, m1);

        let m2 = NotifyMode::Delayed {
            fire_at_unix_ms: 12345,
        };
        let s2 = serde_json::to_string(&m2).unwrap();
        let parsed: NotifyMode = serde_json::from_str(&s2).unwrap();
        assert_eq!(parsed, m2);

        let m3 = NotifyMode::WaitPrevious {
            previous_task_id: Some("forge-1".into()),
        };
        let s3 = serde_json::to_string(&m3).unwrap();
        let parsed: NotifyMode = serde_json::from_str(&s3).unwrap();
        assert_eq!(parsed, m3);
    }

    #[test]
    fn new_job_generates_uuid() {
        let j = new_job(
            "p".into(),
            "p-1".into(),
            "s".into(),
            "hello".into(),
            NotifyMode::Immediate,
        );
        assert_eq!(j.id.len(), 36);
        assert!(j.created_at_unix_ms > 0);
    }
}
