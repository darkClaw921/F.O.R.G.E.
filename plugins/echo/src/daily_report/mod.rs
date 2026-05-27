//! Генерация «Сводки дня» (`daily_report`) для Echo плагина.
//!
//! Фича `forge-b4q`: в конце дня (или по кнопке в настройках) собрать
//! дружелюбную мотивационную сводку рабочего дня на русском в markdown и
//! сохранить её как отдельную пользовательскую сущность `daily_reports`.
//!
//! В отличие от `memory::summarize_day` (внутренний англоязычный артефакт
//! scope=`global_day`, питающий `summarize_project`), `daily_report` — это
//! отдельная сущность с собственной таблицей, русским мотивационным
//! prompt'ом и фиксированной структурой разделов («Что сделано / Где я
//! молодец / На завтра»).
//!
//! Источники данных за день:
//! - Echo-чаты (`memory::collect_day_messages`),
//! - tmux-панели активных сессий (`memory::collect_pane_snapshot`),
//! - git-активность хоста ([`HostApi::collect_git_activity`]).
//!
//! Защита от пустых данных: если за день нет ни сообщений, ни панелей, ни
//! git-коммитов — content ровно `"Сегодня активности не было"` (без вызова
//! Claude). Это маркер пустого дня, который фронтенд может отобразить как есть.

pub mod scheduler;

use std::sync::Arc;

use chrono::NaiveDate;

use echo_host_api::HostApi;

use crate::claude::RunRequest;
use crate::db::repo::daily_reports::{self, DailyReport};
use crate::memory::{collect_day_messages, collect_pane_snapshot, day_bounds_utc, snippet};
use crate::state::EchoState;

/// Русский мотивационный мета-prompt. Просим строго три раздела и точный
/// маркер пустого дня, чтобы поведение совпадало с серверной защитой.
const REPORT_META_PROMPT: &str = "Составь дружелюбную мотивационную сводку моего \
рабочего дня на русском языке в формате markdown. Используй ровно три раздела:\n\
## Что сделано — конкретно, по фактам из чатов, tmux-панелей и git-коммитов.\n\
## Где я молодец — искренне отметь сильные решения, прогресс и удачные ходы.\n\
## На завтра — 1–3 конкретных пункта, что стоит сделать дальше.\n\
Пиши тепло и по-человечески, без воды. Если входных данных нет или они \
тривиальны — ответь ровно: Сегодня активности не было.";

/// Маркер пустого дня. Совпадает с тем, что просим у Claude в prompt'е.
pub(crate) const NO_ACTIVITY_RU: &str = "Сегодня активности не было";

/// Максимальная длина склеенного git-блока в prompt'е (символов).
const GIT_SNIPPET_CAP: usize = 6000;

/// Генерирует сводку за `day` (трактуется как локальная дата) и upsert'ит её
/// в `daily_reports` под ключом `day` (`YYYY-MM-DD`).
///
/// Параметры:
/// - `state` — Echo-состояние (БД + Claude runner).
/// - `host` — plugin boundary для tmux/git.
/// - `day` — день сводки ([`NaiveDate`]); форматируется как `YYYY-MM-DD`.
/// - `source` — `"auto"` (scheduler) или `"manual"` (кнопка/REST).
///
/// Логика:
/// 1. Собрать messages за день, pane-snapshot и git-активность (since = начало дня).
/// 2. Если все три источника пусты — upsert `"Сегодня активности не было"` без
///    обращения к Claude и вернуть запись.
/// 3. Иначе — собрать prompt с русским мета-prompt'ом и блоками данных,
///    прогнать через `state.runner.one_shot`, upsert результат.
///
/// Возвращает итоговую [`DailyReport`] (с стабильным `id` благодаря upsert).
pub async fn generate_report(
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
    day: NaiveDate,
    source: &str,
) -> anyhow::Result<DailyReport> {
    let day_str = day.format("%Y-%m-%d").to_string();

    // `day_bounds_utc` даёт границы дня; для git-активности берём начало дня
    // как нижнюю границу --since.
    let (since_unix, _end_unix) = day_bounds_utc(day);

    let (msgs_text, msgs_count) = collect_day_messages(&state, None, day).await?;
    let pane_text = collect_pane_snapshot(host.as_ref(), None, 100).await;
    let git_text = match host.collect_git_activity(since_unix).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, day = %day_str, "daily_report: collect_git_activity failed");
            String::new()
        }
    };

    let no_data =
        msgs_count == 0 && pane_text.trim().is_empty() && git_text.trim().is_empty();
    if no_data {
        tracing::info!(day = %day_str, "daily_report::generate_report: no data, writing NO_ACTIVITY_RU");
        let report = daily_reports::upsert(&state.db, &day_str, NO_ACTIVITY_RU, source).await?;
        return Ok(report);
    }

    let mut prompt = String::with_capacity(8192);
    prompt.push_str("[task]\n");
    prompt.push_str(REPORT_META_PROMPT);
    prompt.push('\n');
    prompt.push_str(&format!("\n[day]\n{day_str}\n"));
    if !msgs_text.is_empty() {
        prompt.push_str("\n[chat_messages]\n");
        prompt.push_str(&msgs_text);
    }
    if !pane_text.is_empty() {
        prompt.push_str("\n[tmux_panes]\n");
        prompt.push_str(&pane_text);
    }
    if !git_text.trim().is_empty() {
        prompt.push_str("\n[git_activity]\n");
        prompt.push_str(&snippet(&git_text, GIT_SNIPPET_CAP));
        prompt.push('\n');
    }

    let req = RunRequest::new(prompt);
    let res = state.runner.one_shot(req).await?;
    let content = if res.text.trim().is_empty() {
        NO_ACTIVITY_RU.to_string()
    } else {
        res.text.trim().to_string()
    };

    let report = daily_reports::upsert(&state.db, &day_str, &content, source).await?;
    tracing::info!(
        report_id = %report.id,
        day = %day_str,
        source = %source,
        "daily_report::generate_report: upserted"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
    use crate::db::repo::{chats, messages};
    use crate::db::Db;
    use async_trait::async_trait;
    use chrono::Utc;
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
        // collect_git_activity использует default → Ok("").
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
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"## Что сделано\nработа"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":5,"output_tokens":3}}'
"###
    }

    #[tokio::test]
    async fn empty_day_writes_no_activity_ru_without_runner() {
        let dir = tempfile::tempdir().unwrap();
        // CLI, который упал бы при вызове — доказывает, что runner не зовётся.
        let cli = write_mock_cli(&dir, "#!/bin/sh\nexit 1\n");
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let day = NaiveDate::from_ymd_opt(2026, 5, 17).unwrap();
        let report = generate_report(state.clone(), host, day, "auto").await.unwrap();
        assert_eq!(report.content, NO_ACTIVITY_RU);
        assert_eq!(report.day, "2026-05-17");
        assert_eq!(report.source, "auto");

        let stored = daily_reports::get_by_day(&state.db, "2026-05-17")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.content, NO_ACTIVITY_RU);
    }

    #[tokio::test]
    async fn day_with_messages_invokes_runner_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let s = chats::create(&state.db, "test", None, "sonnet").await.unwrap();
        messages::insert(&state.db, &s.id, "user", "Поработали", None, None, 1, 0, 0, 0)
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let report = generate_report(state.clone(), host, today, "manual")
            .await
            .unwrap();
        assert!(report.content.contains("Что сделано"), "got: {}", report.content);
        assert_eq!(report.source, "manual");
    }

    #[tokio::test]
    async fn regenerating_same_day_keeps_stable_id() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let s = chats::create(&state.db, "t", None, "sonnet").await.unwrap();
        messages::insert(&state.db, &s.id, "user", "work", None, None, 1, 0, 0, 0)
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let host2: Arc<dyn HostApi> = Arc::new(StubHost);
        let a = generate_report(state.clone(), host, today, "auto").await.unwrap();
        let b = generate_report(state.clone(), host2, today, "manual").await.unwrap();
        assert_eq!(a.id, b.id, "upsert keeps id stable across regenerations");
        let all = daily_reports::list(&state.db, 100).await.unwrap();
        assert_eq!(all.len(), 1);
    }
}
