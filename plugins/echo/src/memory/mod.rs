//! Memory automation для Echo плагина.
//!
//! Phase 5 — автоматическая суммаризация дневной активности и обновление
//! per-project memories. Используется hourly UTC-day rollover loop'ом
//! ([`scheduler`]) и POST `/api/echo/memories/regenerate` ([`crate::routes`]).
//!
//! Базовая идея:
//!
//! - `summarize_day(state, host, day, project_id)` — собирает все user/assistant
//!   сообщения за `day` (UTC, `YYYY-MM-DD`) и опционально capture-pane активных
//!   сессий проекта, отправляет в Claude с мета-prompt'ом "Summarize key facts
//!   and decisions from this day", upsert'ит результат в `memories` под scope
//!   `global_day` (project_id=None) или `project_day` (project_id=Some).
//! - `summarize_project(state, host, project_id)` — собирает 20 последних
//!   `project_day` memories + ключевые messages, отправляет в Claude с
//!   мета-prompt'ом "Build/update a stable project memory", upsert'ит результат
//!   под scope `project`.
//!
//! Защита от пустых данных: если за день нет сообщений и capture-pane пуст —
//! upsert'им memory с content "No activity" (источник `auto`). Это позволяет
//! последующим прогонам пропустить регенерацию, если ничего не изменилось.
//!
//! Возвращает `Memory.id` после upsert'а — caller (route) может вернуть его
//! фронтенду для подсветки.

pub mod scheduler;

use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};

use echo_host_api::HostApi;

use crate::claude::RunRequest;
use crate::db::repo::{memories, messages};
use crate::state::EchoState;

/// Максимум сообщений за день, которые подмешиваем в prompt. Cap нужен,
/// чтобы избежать прокидывания мегабайтных историй в Claude.
const MAX_DAY_MESSAGES: usize = 200;

/// Максимум project_day memories, которые подмешиваем в `summarize_project`.
const MAX_PROJECT_DAY_MEMORIES: usize = 20;

/// Максимум длины одного message-сниппета в prompt'е (символов).
const MESSAGE_SNIPPET_CAP: usize = 800;

/// Мета-prompt для дневной суммаризации.
const DAY_META_PROMPT: &str = "Summarize key facts and decisions from this day. \
Output concise markdown (≤ 1500 tokens). Focus on what changed: features, bugs, \
decisions, files touched. Be specific. If the input is empty or trivial, reply with \
exactly: No activity.";

/// Мета-prompt для проектной памяти.
const PROJECT_META_PROMPT: &str = "Build/update a stable project memory: \
architecture, decisions, current focus. Output concise markdown (≤ 2000 tokens). \
Avoid ephemeral details — prefer durable facts (data flows, ownership, conventions). \
If input is trivial, reply with exactly: No project context.";

/// Текст-маркер «нет активности», который мы сохраняем при отсутствии данных.
const NO_ACTIVITY: &str = "No activity";

/// Параметры дня в UTC. Принимаем `chrono::NaiveDate` чтобы caller'ы из
/// разных таймзон были вынуждены явно конвертировать.
pub(crate) fn day_bounds_utc(day: NaiveDate) -> (i64, i64) {
    let start = day.and_hms_opt(0, 0, 0).expect("valid time");
    let next = start + chrono::Duration::days(1);
    let start_ts = Utc.from_utc_datetime(&start).timestamp();
    let end_ts = Utc.from_utc_datetime(&next).timestamp();
    (start_ts, end_ts)
}

/// Усекает текст до `cap` символов с маркером "…" при отсечении.
pub(crate) fn snippet(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.trim().to_string();
    }
    let mut out: String = s.chars().take(cap).collect();
    out.push('…');
    out.trim().to_string()
}

/// Достаёт messages за день для всех чатов опционально-фильтрованных
/// `project_id`. Возвращает (snippet-collected text, original count).
///
/// Реализация — простой `SELECT` через repo (нет специального API на день,
/// поэтому фильтруем здесь по `created_at`).
pub(crate) async fn collect_day_messages(
    state: &Arc<EchoState>,
    project_id: Option<&str>,
    day: NaiveDate,
) -> anyhow::Result<(String, usize)> {
    let (start_ts, end_ts) = day_bounds_utc(day);
    let project = project_id.map(|s| s.to_string());

    let rows: Vec<(String, String, String, i64)> = state
        .db
        .conn()
        .call(move |c| {
            let sql = if project.is_some() {
                "SELECT m.id, m.role, m.content, m.created_at \
                 FROM messages m JOIN chat_sessions s ON s.id = m.session_id \
                 WHERE s.project_id = ?1 AND m.created_at >= ?2 AND m.created_at < ?3 \
                 ORDER BY m.created_at ASC LIMIT ?4"
            } else {
                "SELECT id, role, content, created_at FROM messages \
                 WHERE created_at >= ?2 AND created_at < ?3 \
                 ORDER BY created_at ASC LIMIT ?4"
            };
            let mut stmt = c.prepare(sql)?;
            let mut rows: Vec<(String, String, String, i64)> = Vec::new();
            if let Some(pid) = project {
                let mut it = stmt.query(rusqlite::params![
                    pid,
                    start_ts,
                    end_ts,
                    MAX_DAY_MESSAGES as i64
                ])?;
                while let Some(row) = it.next()? {
                    rows.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
                }
            } else {
                let mut it = stmt.query(rusqlite::params![
                    Option::<String>::None,
                    start_ts,
                    end_ts,
                    MAX_DAY_MESSAGES as i64
                ])?;
                while let Some(row) = it.next()? {
                    rows.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
                }
            }
            Ok(rows)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memory: collect_day_messages: {e}"))?;

    let count = rows.len();
    let mut out = String::new();
    for (_id, role, content, _ts) in rows {
        if content.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("- [{}] {}\n", role, snippet(&content, MESSAGE_SNIPPET_CAP)));
    }
    Ok((out, count))
}

/// Собирает текст из capture-pane'ов активных сессий проекта (или всех, если
/// project_id=None). Используется в `summarize_day` для grounding.
pub(crate) async fn collect_pane_snapshot(
    host: &dyn HostApi,
    _project_id: Option<&str>,
    lines: i32,
) -> String {
    let mut out = String::new();
    match host.list_sessions().await {
        Ok(sessions) => {
            for s in sessions {
                match host.capture_pane_full(&s.name, lines).await {
                    Ok(text) if !text.trim().is_empty() => {
                        out.push_str(&format!(
                            "## session: {} ({} windows)\n{}\n---\n",
                            s.name,
                            s.windows,
                            snippet(&text, 4000)
                        ));
                    }
                    _ => {}
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "memory: list_sessions failed");
        }
    }
    out
}

/// Суммаризует один день — global (project_id=None) или per-project.
///
/// Логика:
/// 1. Собрать messages за `day` (UTC).
/// 2. Собрать pane-snapshot (опционально).
/// 3. Если данных нет — upsert memory с content="No activity" и source="auto".
/// 4. Иначе — отправить в Claude с мета-prompt'ом, upsert результат.
///
/// Возвращает `Memory.id` (стабильный для одного и того же ключа благодаря
/// upsert-семантике).
pub async fn summarize_day(
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
    day: NaiveDate,
    project_id: Option<&str>,
) -> anyhow::Result<String> {
    let day_str = day.format("%Y-%m-%d").to_string();
    let (msgs_text, msgs_count) = collect_day_messages(&state, project_id, day).await?;
    let pane_text = collect_pane_snapshot(host.as_ref(), project_id, 100).await;

    let scope = if project_id.is_some() {
        memories::MemoryScope::ProjectDay
    } else {
        memories::MemoryScope::GlobalDay
    };

    // Пустые данные → No activity. Это позволяет фронтенду понимать, что
    // прогон был выполнен и день пустой (vs «ещё не считали»).
    if msgs_count == 0 && pane_text.trim().is_empty() {
        tracing::info!(
            day = %day_str,
            project_id = ?project_id,
            "memory::summarize_day: no data, writing No activity"
        );
        let m = memories::upsert(
            &state.db,
            scope,
            project_id,
            Some(&day_str),
            NO_ACTIVITY,
            "auto",
        )
        .await?;
        return Ok(m.id);
    }

    let mut prompt = String::with_capacity(4096);
    prompt.push_str("[task]\n");
    prompt.push_str(DAY_META_PROMPT);
    prompt.push('\n');
    prompt.push_str(&format!("\n[day]\n{day_str}\n"));
    if let Some(pid) = project_id {
        prompt.push_str(&format!("\n[project]\n{pid}\n"));
    }
    if !msgs_text.is_empty() {
        prompt.push_str("\n[messages]\n");
        prompt.push_str(&msgs_text);
    }
    if !pane_text.is_empty() {
        prompt.push_str("\n[tmux_panes]\n");
        prompt.push_str(&pane_text);
    }

    let req = RunRequest::new(prompt);
    let res = state.runner.one_shot(req).await?;
    let content = if res.text.trim().is_empty() {
        NO_ACTIVITY.to_string()
    } else {
        res.text
    };

    let m = memories::upsert(
        &state.db,
        scope,
        project_id,
        Some(&day_str),
        content.trim(),
        "auto",
    )
    .await?;
    tracing::info!(
        memory_id = %m.id,
        day = %day_str,
        project_id = ?project_id,
        "memory::summarize_day: upserted"
    );
    Ok(m.id)
}

/// Собирает/обновляет стабильную проектную память.
///
/// Логика:
/// 1. Подтянуть N последних `project_day` memories для project_id.
/// 2. Подтянуть последние N сообщений (для свежего контекста).
/// 3. Если нет ничего — upsert "No project context", source="auto".
/// 4. Иначе — отправить в Claude с мета-prompt'ом, upsert под scope=project.
pub async fn summarize_project(
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
    project_id: &str,
) -> anyhow::Result<String> {
    // 1) project_day memories (последние N).
    let day_mems = memories::list(
        &state.db,
        Some(memories::MemoryScope::ProjectDay),
        Some(project_id),
        None,
    )
    .await?;
    let day_mems: Vec<_> = day_mems.into_iter().take(MAX_PROJECT_DAY_MEMORIES).collect();

    // 2) ключевые messages за последние 7 дней.
    let today = Utc::now().date_naive();
    let week_ago = today - chrono::Duration::days(7);
    let (msgs_text, msgs_count) =
        collect_week_messages(&state, Some(project_id), week_ago, today).await?;

    let pane_text = collect_pane_snapshot(host.as_ref(), Some(project_id), 80).await;

    if day_mems.is_empty() && msgs_count == 0 && pane_text.trim().is_empty() {
        tracing::info!(project_id, "memory::summarize_project: no data, writing No project context");
        let m = memories::upsert(
            &state.db,
            memories::MemoryScope::Project,
            Some(project_id),
            None,
            "No project context",
            "auto",
        )
        .await?;
        return Ok(m.id);
    }

    let mut prompt = String::with_capacity(8192);
    prompt.push_str("[task]\n");
    prompt.push_str(PROJECT_META_PROMPT);
    prompt.push('\n');
    prompt.push_str(&format!("\n[project]\n{project_id}\n"));
    if !day_mems.is_empty() {
        prompt.push_str("\n[recent_day_summaries]\n");
        for m in &day_mems {
            prompt.push_str(&format!(
                "### {}\n{}\n\n",
                m.day.as_deref().unwrap_or("?"),
                snippet(&m.content, 2000)
            ));
        }
    }
    if !msgs_text.is_empty() {
        prompt.push_str("\n[recent_messages]\n");
        prompt.push_str(&msgs_text);
    }
    if !pane_text.is_empty() {
        prompt.push_str("\n[tmux_panes]\n");
        prompt.push_str(&pane_text);
    }

    let req = RunRequest::new(prompt);
    let res = state.runner.one_shot(req).await?;
    let content = if res.text.trim().is_empty() {
        "No project context".to_string()
    } else {
        res.text
    };

    let m = memories::upsert(
        &state.db,
        memories::MemoryScope::Project,
        Some(project_id),
        None,
        content.trim(),
        "auto",
    )
    .await?;
    tracing::info!(
        memory_id = %m.id,
        project_id,
        "memory::summarize_project: upserted"
    );
    Ok(m.id)
}

/// Собирает сообщения за диапазон [start_day, end_day) — используется
/// `summarize_project` для контекста последней недели.
async fn collect_week_messages(
    state: &Arc<EchoState>,
    project_id: Option<&str>,
    start_day: NaiveDate,
    end_day: NaiveDate,
) -> anyhow::Result<(String, usize)> {
    let (start_ts, _) = day_bounds_utc(start_day);
    let (_, end_ts) = day_bounds_utc(end_day);
    let project = project_id.map(|s| s.to_string());

    let rows: Vec<(String, String, i64)> = state
        .db
        .conn()
        .call(move |c| {
            let sql = if project.is_some() {
                "SELECT m.role, m.content, m.created_at \
                 FROM messages m JOIN chat_sessions s ON s.id = m.session_id \
                 WHERE s.project_id = ?1 AND m.created_at >= ?2 AND m.created_at < ?3 \
                 ORDER BY m.created_at DESC LIMIT ?4"
            } else {
                "SELECT role, content, created_at FROM messages \
                 WHERE created_at >= ?2 AND created_at < ?3 \
                 ORDER BY created_at DESC LIMIT ?4"
            };
            let mut stmt = c.prepare(sql)?;
            let mut rows: Vec<(String, String, i64)> = Vec::new();
            if let Some(pid) = project {
                let mut it = stmt.query(rusqlite::params![
                    pid,
                    start_ts,
                    end_ts,
                    MAX_DAY_MESSAGES as i64
                ])?;
                while let Some(row) = it.next()? {
                    rows.push((row.get(0)?, row.get(1)?, row.get(2)?));
                }
            } else {
                let mut it = stmt.query(rusqlite::params![
                    Option::<String>::None,
                    start_ts,
                    end_ts,
                    MAX_DAY_MESSAGES as i64
                ])?;
                while let Some(row) = it.next()? {
                    rows.push((row.get(0)?, row.get(1)?, row.get(2)?));
                }
            }
            Ok(rows)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memory: collect_week_messages: {e}"))?;

    let count = rows.len();
    let mut out = String::new();
    for (role, content, _ts) in rows {
        if content.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("- [{}] {}\n", role, snippet(&content, MESSAGE_SNIPPET_CAP)));
    }
    Ok((out, count))
}

// -------- expose helper для тестов scheduler'а --------

/// Тестовый helper, не для production. Удалён `#[cfg(test)]` потому что
/// `scheduler` тоже хочет использовать `day_bounds_utc` для расчёта вчерашнего
/// дня в своих тестах.
#[doc(hidden)]
pub fn _day_bounds_utc(day: NaiveDate) -> (i64, i64) {
    day_bounds_utc(day)
}

// Чтобы линкер не жаловался на неиспользованную функцию из `messages` repo:
#[allow(dead_code)]
fn _import_messages_keepalive() {
    let _ = messages::insert;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::repo::chats;
    use crate::db::Db;
    use async_trait::async_trait;
    use echo_host_api::SessionInfo;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

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

    fn write_mock_cli(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    async fn make_state(cli: PathBuf) -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        Arc::new(EchoState::new(Arc::new(db), runner))
    }

    fn mock_summary_script() -> &'static str {
        r###"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Mock summary."}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":5,"output_tokens":3}}'
"###
    }

    #[tokio::test]
    async fn summarize_day_with_no_data_writes_no_activity() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let id = summarize_day(state.clone(), host, NaiveDate::from_ymd_opt(2026, 5, 17).unwrap(), None)
            .await
            .unwrap();
        let m = memories::get(&state.db, &id).await.unwrap().unwrap();
        assert_eq!(m.content, NO_ACTIVITY);
        assert_eq!(m.scope, "global_day");
        assert_eq!(m.source, "auto");
    }

    #[tokio::test]
    async fn summarize_day_with_messages_invokes_runner_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        // Создаём чат и сообщение сегодня.
        let s = chats::create(&state.db, "test", None, "sonnet").await.unwrap();
        messages::insert(&state.db, &s.id, "user", "Hello, day!", None, None, 1, 0, 0, 0)
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let id = summarize_day(state.clone(), host, today, None).await.unwrap();
        let m = memories::get(&state.db, &id).await.unwrap().unwrap();
        assert!(m.content.contains("Mock summary"), "got: {}", m.content);
        assert_eq!(m.scope, "global_day");
    }

    #[tokio::test]
    async fn summarize_day_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let s = chats::create(&state.db, "p-chat", Some("p1"), "sonnet").await.unwrap();
        messages::insert(&state.db, &s.id, "user", "Project work", None, None, 1, 0, 0, 0)
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let id = summarize_day(state.clone(), host, today, Some("p1"))
            .await
            .unwrap();
        let m = memories::get(&state.db, &id).await.unwrap().unwrap();
        assert_eq!(m.scope, "project_day");
        assert_eq!(m.project_id.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn summarize_project_with_no_data_writes_no_project_context() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let id = summarize_project(state.clone(), host, "p_empty").await.unwrap();
        let m = memories::get(&state.db, &id).await.unwrap().unwrap();
        assert_eq!(m.scope, "project");
        assert_eq!(m.content, "No project context");
    }

    #[tokio::test]
    async fn summarize_project_aggregates_day_memories() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        // Создаём пару project_day memories.
        memories::upsert(
            &state.db,
            memories::MemoryScope::ProjectDay,
            Some("px"),
            Some("2026-05-15"),
            "Day 15 summary",
            "auto",
        )
        .await
        .unwrap();

        let id = summarize_project(state.clone(), host, "px").await.unwrap();
        let m = memories::get(&state.db, &id).await.unwrap().unwrap();
        // Должна быть запись в scope=project под px.
        assert_eq!(m.scope, "project");
        assert_eq!(m.project_id.as_deref(), Some("px"));
        // Контент — то, что вернул мок-CLI.
        assert!(m.content.contains("Mock summary"));
    }

    #[test]
    fn snippet_caps_long_strings() {
        let long = "a".repeat(1000);
        let s = snippet(&long, 100);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 101);
    }
}
