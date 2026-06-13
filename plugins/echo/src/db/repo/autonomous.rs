//! CRUD для `autonomous_tasks` и `task_runs`.
//!
//! - `AutonomousTask` — описание планируемой задачи (prompt + interval +
//!   model). `enabled` = `0/1` (INTEGER), `next_run_at` — момент следующего
//!   запуска (используется в составном index'е для O(log n) подбора
//!   due-задач).
//! - `TaskRun` — лог одного выполнения. `status` CHECK
//!   ('running','success','error','cancelled'); `finished_at` NULL пока
//!   running.
//! - `find_due` — рабочая лошадка Phase 4 scheduler'а:
//!   `WHERE enabled=1 AND next_run_at IS NOT NULL AND next_run_at<=now`
//!   с лимитом для batch'ей.

use serde::Serialize;

use crate::db::Db;

#[derive(Debug, Clone, Serialize)]
pub struct AutonomousTask {
    pub id: String,
    pub name: String,
    pub prompt_template: String,
    pub interval_seconds: i64,
    pub model: String,
    pub enabled: bool,
    pub project_id: Option<String>,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub result_message_id: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub error: Option<String>,
}

/// Patch для частичного обновления task.
/// `None` поле = «не менять».
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub name: Option<String>,
    pub prompt_template: Option<String>,
    pub interval_seconds: Option<i64>,
    pub model: Option<String>,
    pub enabled: Option<bool>,
}

pub async fn create_task(
    db: &Db,
    name: &str,
    prompt_template: &str,
    interval_seconds: i64,
    model: &str,
    project_id: Option<&str>,
) -> anyhow::Result<AutonomousTask> {
    let t = AutonomousTask {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        prompt_template: prompt_template.to_string(),
        interval_seconds,
        model: model.to_string(),
        enabled: true,
        project_id: project_id.map(|s| s.to_string()),
        last_run_at: None,
        next_run_at: Some(chrono::Utc::now().timestamp() + interval_seconds),
        created_at: chrono::Utc::now().timestamp(),
    };
    let row = t.clone();
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO autonomous_tasks(\
                   id, name, prompt_template, interval_seconds, model, enabled,\
                   project_id, last_run_at, next_run_at, created_at\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    row.id,
                    row.name,
                    row.prompt_template,
                    row.interval_seconds,
                    row.model,
                    row.enabled as i64,
                    row.project_id,
                    row.last_run_at,
                    row.next_run_at,
                    row.created_at,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::create_task: {e}"))?;
    Ok(t)
}

pub async fn list_tasks(db: &Db, enabled_only: bool) -> anyhow::Result<Vec<AutonomousTask>> {
    db.conn()
        .call(move |c| {
            let sql = if enabled_only {
                "SELECT id, name, prompt_template, interval_seconds, model, enabled,\
                        project_id, last_run_at, next_run_at, created_at \
                 FROM autonomous_tasks WHERE enabled = 1 ORDER BY created_at DESC"
            } else {
                "SELECT id, name, prompt_template, interval_seconds, model, enabled,\
                        project_id, last_run_at, next_run_at, created_at \
                 FROM autonomous_tasks ORDER BY created_at DESC"
            };
            let mut stmt = c.prepare(sql)?;
            let it = stmt.query_map([], row_to_task)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::list_tasks: {e}"))
}

pub async fn get_task(db: &Db, id: &str) -> anyhow::Result<Option<AutonomousTask>> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id, name, prompt_template, interval_seconds, model, enabled,\
                            project_id, last_run_at, next_run_at, created_at \
                     FROM autonomous_tasks WHERE id = ?1",
                    rusqlite::params![id],
                    row_to_task,
                )
                .ok();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::get_task: {e}"))
}

pub async fn update_task(db: &Db, id: &str, patch: TaskPatch) -> anyhow::Result<usize> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            // COALESCE(?, column) — если параметр NULL, оставляем старое значение.
            let n = c.execute(
                "UPDATE autonomous_tasks SET \
                   name             = COALESCE(?1, name),\
                   prompt_template  = COALESCE(?2, prompt_template),\
                   interval_seconds = COALESCE(?3, interval_seconds),\
                   model            = COALESCE(?4, model),\
                   enabled          = COALESCE(?5, enabled) \
                 WHERE id = ?6",
                rusqlite::params![
                    patch.name,
                    patch.prompt_template,
                    patch.interval_seconds,
                    patch.model,
                    patch.enabled.map(|b| b as i64),
                    id,
                ],
            )?;
            Ok(n)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::update_task: {e}"))
}

pub async fn delete_task(db: &Db, id: &str) -> anyhow::Result<usize> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let n = c.execute(
                "DELETE FROM autonomous_tasks WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(n)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::delete_task: {e}"))
}

pub async fn set_next_run(db: &Db, id: &str, next_run_at: i64) -> anyhow::Result<()> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE autonomous_tasks SET next_run_at = ?1 WHERE id = ?2",
                rusqlite::params![next_run_at, id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::set_next_run: {e}"))?;
    Ok(())
}

/// Максимум задач, выбираемых за один scheduler-tick. Защищает планировщик
/// от всплеска нагрузки, если после долгого простоя/рассинхрона часов
/// одновременно «просрочилось» очень много задач: запускаем их пачками по
/// тикам, а не все сразу (что забило бы семафор Claude и БД).
pub const FIND_DUE_LIMIT: i64 = 64;

/// Scheduler-tick: вернёт enabled-задачи с `next_run_at <= now_unix`,
/// не более [`FIND_DUE_LIMIT`] за вызов (самые «просроченные» первыми).
/// Использует составной index `idx_autonomous_enabled_next`.
pub async fn find_due(db: &Db, now_unix: i64) -> anyhow::Result<Vec<AutonomousTask>> {
    db.conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, name, prompt_template, interval_seconds, model, enabled,\
                        project_id, last_run_at, next_run_at, created_at \
                 FROM autonomous_tasks \
                 WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
                 ORDER BY next_run_at ASC \
                 LIMIT ?2",
            )?;
            let it = stmt.query_map(rusqlite::params![now_unix, FIND_DUE_LIMIT], row_to_task)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::find_due: {e}"))
}

pub async fn insert_run(db: &Db, task_id: &str, started_at: i64) -> anyhow::Result<TaskRun> {
    let r = TaskRun {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        started_at,
        finished_at: None,
        status: "running".to_string(),
        result_message_id: None,
        tokens_in: 0,
        tokens_out: 0,
        error: None,
    };
    let row = r.clone();
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO task_runs(id, task_id, started_at, status) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![row.id, row.task_id, row.started_at, row.status],
            )?;
            // Зеркалим started_at в task.last_run_at для удобства.
            c.execute(
                "UPDATE autonomous_tasks SET last_run_at = ?1 WHERE id = ?2",
                rusqlite::params![started_at, row.task_id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::insert_run: {e}"))?;
    Ok(r)
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_run(
    db: &Db,
    run_id: &str,
    status: &str,
    result_message_id: Option<&str>,
    tokens_in: i64,
    tokens_out: i64,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let run_id = run_id.to_string();
    let status = status.to_string();
    let result = result_message_id.map(|s| s.to_string());
    let err = error.map(|s| s.to_string());
    let now = chrono::Utc::now().timestamp();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE task_runs SET \
                   finished_at = ?1, status = ?2, result_message_id = ?3,\
                   tokens_in = ?4, tokens_out = ?5, error = ?6 \
                 WHERE id = ?7",
                rusqlite::params![now, status, result, tokens_in, tokens_out, err, run_id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::finish_run: {e}"))?;
    Ok(())
}

/// Phase 6 — суммирует `tokens_in + tokens_out` для всех `task_runs`,
/// которые `started_at >= since_unix`. Используется для дневного cap'а:
/// при превышении автономные задачи отключаются.
///
/// Considered все запуски (success / error / running) — running обычно
/// ещё не имеют записанных токенов (они 0), но мы не делаем доп. фильтр —
/// проще и идемпотентнее.
pub async fn sum_tokens_since(db: &Db, since_unix: i64) -> anyhow::Result<u64> {
    db.conn()
        .call(move |c| {
            let n: i64 = c
                .query_row(
                    "SELECT COALESCE(SUM(tokens_in + tokens_out), 0) \
                     FROM task_runs WHERE started_at >= ?1",
                    rusqlite::params![since_unix],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            Ok(n.max(0) as u64)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::sum_tokens_since: {e}"))
}

pub async fn list_runs(db: &Db, task_id: &str, limit: i64) -> anyhow::Result<Vec<TaskRun>> {
    let task_id = task_id.to_string();
    db.conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, task_id, started_at, finished_at, status,\
                        result_message_id, tokens_in, tokens_out, error \
                 FROM task_runs WHERE task_id = ?1 \
                 ORDER BY started_at DESC LIMIT ?2",
            )?;
            let it = stmt.query_map(rusqlite::params![task_id, limit], row_to_run)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("autonomous::list_runs: {e}"))
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutonomousTask> {
    Ok(AutonomousTask {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt_template: row.get(2)?,
        interval_seconds: row.get(3)?,
        model: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        project_id: row.get(6)?,
        last_run_at: row.get(7)?,
        next_run_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRun> {
    Ok(TaskRun {
        id: row.get(0)?,
        task_id: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        status: row.get(4)?,
        result_message_id: row.get(5)?,
        tokens_in: row.get(6)?,
        tokens_out: row.get(7)?,
        error: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> Db {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    #[tokio::test]
    async fn create_get_update_delete_task() {
        let db = fresh().await;
        let t = create_task(&db, "daily", "ping", 60, "sonnet-4", None)
            .await
            .unwrap();
        assert!(t.enabled);
        assert_eq!(get_task(&db, &t.id).await.unwrap().unwrap().name, "daily");

        update_task(
            &db,
            &t.id,
            TaskPatch {
                name: Some("renamed".into()),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t2 = get_task(&db, &t.id).await.unwrap().unwrap();
        assert_eq!(t2.name, "renamed");
        assert!(!t2.enabled);

        assert_eq!(delete_task(&db, &t.id).await.unwrap(), 1);
        assert!(get_task(&db, &t.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn find_due_scheduler_tick() {
        let db = fresh().await;
        let now = chrono::Utc::now().timestamp();
        let t = create_task(&db, "x", "p", 1, "m", None).await.unwrap();
        // Принудительно ставим next_run_at в прошлое.
        set_next_run(&db, &t.id, now - 10).await.unwrap();
        let due = find_due(&db, now).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, t.id);

        // Disable → больше не due.
        update_task(
            &db,
            &t.id,
            TaskPatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(find_due(&db, now).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_lifecycle() {
        let db = fresh().await;
        let now = chrono::Utc::now().timestamp();
        let t = create_task(&db, "x", "p", 60, "m", None).await.unwrap();
        let r = insert_run(&db, &t.id, now).await.unwrap();
        assert_eq!(r.status, "running");
        assert!(r.finished_at.is_none());
        let t2 = get_task(&db, &t.id).await.unwrap().unwrap();
        assert_eq!(t2.last_run_at, Some(now));

        finish_run(&db, &r.id, "success", Some("msg-1"), 100, 50, None)
            .await
            .unwrap();
        let runs = list_runs(&db, &t.id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert_eq!(runs[0].tokens_in, 100);
        assert_eq!(runs[0].result_message_id.as_deref(), Some("msg-1"));
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn sum_tokens_since_aggregates_only_recent_runs() {
        let db = fresh().await;
        let t = create_task(&db, "x", "p", 60, "m", None).await.unwrap();
        // Старый run (давно).
        let r_old = insert_run(&db, &t.id, 100).await.unwrap();
        finish_run(&db, &r_old.id, "success", None, 50, 10, None)
            .await
            .unwrap();
        // Свежий run.
        let now = chrono::Utc::now().timestamp();
        let r_new = insert_run(&db, &t.id, now).await.unwrap();
        finish_run(&db, &r_new.id, "success", None, 200, 30, None)
            .await
            .unwrap();

        // Окно: только сегодняшние runs.
        let day_start = now - 3600;
        let sum = sum_tokens_since(&db, day_start).await.unwrap();
        assert_eq!(sum, 200 + 30);

        // Окно с захватом всего — оба runs.
        let sum_all = sum_tokens_since(&db, 0).await.unwrap();
        assert_eq!(sum_all, 50 + 10 + 200 + 30);
    }

    #[tokio::test]
    async fn cascade_delete_task_drops_runs() {
        let db = fresh().await;
        let t = create_task(&db, "x", "p", 60, "m", None).await.unwrap();
        insert_run(&db, &t.id, 0).await.unwrap();
        delete_task(&db, &t.id).await.unwrap();
        assert!(list_runs(&db, &t.id, 10).await.unwrap().is_empty());
    }
}
