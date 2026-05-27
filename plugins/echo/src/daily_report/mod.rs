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
use serde::{Deserialize, Serialize};

use echo_host_api::{HostApi, ProjectActivity};

use crate::claude::RunRequest;
use crate::db::repo::daily_reports::{self, DailyReport};
use crate::memory::{collect_day_messages, collect_pane_snapshot, day_bounds_utc, snippet};
use crate::state::EchoState;

/// Дефолтный приоритет предлагаемой задачи (P2 — средний).
fn default_priority() -> i64 {
    2
}

/// Одна предлагаемая задача внутри проекта.
///
/// Формируется LLM на основе git-активности дня и контекста чатов/панелей.
/// `description` и `priority` опциональны в JSON-ответе (дефолты применяются
/// при десериализации), чтобы модель могла вернуть минимальный объект.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SuggestedTask {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_priority")]
    priority: i64,
}

/// Предложения задач по одному проекту.
///
/// `project_path` — ключ проекта (git-корень); ДОЛЖЕН совпадать с `path`,
/// переданным в prompt, потому что используется как `path` при создании TODO
/// через `POST /api/todos`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectSuggestions {
    project_path: String,
    project_name: String,
    tasks: Vec<SuggestedTask>,
}

/// Ключ в `app_settings` для пользовательского оверрайда промпта отчёта.
/// Если значение пустое/отсутствует — используется [`REPORT_META_PROMPT`].
pub(crate) const PROMPT_KEY_REPORT: &str = "daily_report.report_prompt";

/// Ключ в `app_settings` для пользовательского оверрайда промпта предложений.
/// Если значение пустое/отсутствует — используется [`SUGGEST_META_PROMPT`].
pub(crate) const PROMPT_KEY_SUGGEST: &str = "daily_report.suggest_prompt";

/// Мета-prompt (русский) для генерации предложений задач по проектам.
/// Требует строго JSON-массив без markdown и пояснений.
pub(crate) const SUGGEST_META_PROMPT: &str = "Проанализируй ВЕСЬ контекст рабочего дня вместе \
— переписку в чатах, содержимое tmux-панелей и git-активность — и для каждого \
проекта предложи 1–3 задачи на будущее.\n\
ЦЕЛЬ: подсветить НЕ-очевидное и то, что легко упустить. Ищи забытые и отложенные \
вещи, незакрытые хвосты и TODO/FIXME, потенциальные улучшения и рефакторинг, \
недостающие тесты и документацию, технический долг, обнаруженные но не \
исправленные баги, идеи что стоит добавить или доработать.\n\
НЕ пересказывай коммиты и НЕ дублируй уже сделанное за день — предлагай \
осмысленный следующий шаг. Каждая задача должна быть конкретной, прикладной и \
явно вытекать из контекста (а не абстрактным советом). В description коротко \
поясни, ПОЧЕМУ это важно или откуда взялось.\n\
Верни СТРОГО JSON-массив объектов вида:\n\
[{\"project_path\":\"...\",\"project_name\":\"...\",\"tasks\":[{\"title\":\"...\",\
\"description\":\"...\",\"priority\":2}]}]\n\
Без markdown, без тройных бэктиков, без пояснений — только JSON. Значение \
project_path в ответе ДОЛЖНО точно совпадать с path соответствующего проекта \
из входных данных. priority — целое 0..4 (по умолчанию 2). Если по проекту нет \
осмысленных идей — верни для него пустой массив tasks.";

/// Максимальная длина git_log одного проекта в prompt'е (символов).
const PROJECT_GIT_SNIPPET_CAP: usize = 2000;

/// Робастно парсит JSON-ответ модели в `Vec<ProjectSuggestions>`:
/// снимает возможные ```json-обёртки, вырезает подстроку от первого `[` до
/// последнего `]`. При любой ошибке → пустой вектор (НЕ паникует).
fn parse_suggestions_response(raw: &str) -> Vec<ProjectSuggestions> {
    let trimmed = raw.trim();
    // Снять тройные бэктики/язык-метку, если модель всё же обернула ответ.
    let without_fences = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let start = without_fences.find('[');
    let end = without_fences.rfind(']');
    let slice = match (start, end) {
        (Some(s), Some(e)) if e >= s => &without_fences[s..=e],
        _ => return Vec::new(),
    };

    match serde_json::from_str::<Vec<ProjectSuggestions>>(slice) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "daily_report: failed to parse suggestions JSON");
            Vec::new()
        }
    }
}

/// Максимальная длина блока чатов/панелей в prompt'е предложений (символов).
const SUGGEST_CONTEXT_CAP: usize = 6000;

/// Генерирует предложения задач по проектам через отдельный `one_shot`.
///
/// На вход подаётся ВЕСЬ контекст дня: сообщения чатов (`msgs_text`),
/// snapshot tmux-панелей (`pane_text`) и per-project git-активность
/// (`projects`). Промпт нацелен на не-очевидное (забытые/отложенные вещи,
/// улучшения, технический долг), а не на пересказ коммитов.
///
/// Если проектов нет — runner НЕ вызывается, возвращается пустой вектор.
/// Любая ошибка генерации/парсинга деградирует до пустого вектора (основной
/// отчёт важнее).
async fn generate_suggestions(
    state: &Arc<EchoState>,
    projects: &[ProjectActivity],
    msgs_text: &str,
    pane_text: &str,
    day_str: &str,
) -> Vec<ProjectSuggestions> {
    if projects.is_empty() {
        return Vec::new();
    }

    // Эффективный промпт: пользовательский оверрайд из app_settings, иначе дефолт.
    let suggest_prompt = crate::db::repo::app_settings::get(&state.db, PROMPT_KEY_SUGGEST)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| SUGGEST_META_PROMPT.to_string());

    let mut prompt = String::with_capacity(8192);
    prompt.push_str("[task]\n");
    prompt.push_str(&suggest_prompt);
    prompt.push_str(&format!("\n\n[day]\n{day_str}\n"));
    prompt.push_str("\n[projects]\n");
    for p in projects {
        prompt.push_str(&format!("\n## project\npath: {}\nname: {}\n", p.path, p.name));
        if p.git_log.trim().is_empty() {
            prompt.push_str("git_log: (нет коммитов за день)\n");
        } else {
            prompt.push_str("git_log:\n");
            prompt.push_str(&snippet(p.git_log.trim(), PROJECT_GIT_SNIPPET_CAP));
            prompt.push('\n');
        }
    }
    // Общий контекст дня — чтобы модель видела намерения/обсуждения/ошибки,
    // а не только итоговые коммиты. Это и даёт не-очевидные предложения.
    if !msgs_text.trim().is_empty() {
        prompt.push_str("\n[chat_messages]\n");
        prompt.push_str(&snippet(msgs_text.trim(), SUGGEST_CONTEXT_CAP));
        prompt.push('\n');
    }
    if !pane_text.trim().is_empty() {
        prompt.push_str("\n[tmux_panes]\n");
        prompt.push_str(&snippet(pane_text.trim(), SUGGEST_CONTEXT_CAP));
        prompt.push('\n');
    }

    let req = RunRequest::new(prompt);
    match state.runner.one_shot(req).await {
        Ok(res) => parse_suggestions_response(&res.text),
        Err(e) => {
            tracing::warn!(error = %e, day = %day_str, "daily_report: suggestions one_shot failed");
            Vec::new()
        }
    }
}

/// Русский мотивационный мета-prompt. Просим строго три раздела и точный
/// маркер пустого дня, чтобы поведение совпадало с серверной защитой.
pub(crate) const REPORT_META_PROMPT: &str = "Составь дружелюбную мотивационную сводку моего \
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
        // Пустой день — предложений нет.
        let empty = serde_json::Value::Array(Vec::new());
        let report =
            daily_reports::upsert(&state.db, &day_str, NO_ACTIVITY_RU, source, &empty).await?;
        return Ok(report);
    }

    // Эффективный промпт: пользовательский оверрайд из app_settings, иначе дефолт.
    let report_prompt = crate::db::repo::app_settings::get(&state.db, PROMPT_KEY_REPORT)
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| REPORT_META_PROMPT.to_string());

    let mut prompt = String::with_capacity(8192);
    prompt.push_str("[task]\n");
    prompt.push_str(&report_prompt);
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

    // Предложения задач НЕ зависят от текста основного отчёта, поэтому обе
    // LLM-генерации гоним ПАРАЛЛЕЛЬНО (runner-семафор это допускает). Иначе два
    // последовательных вызова Claude легко упираются в HTTP-таймаут generate.
    let projects = match host.collect_project_activity(since_unix).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, day = %day_str, "daily_report: collect_project_activity failed");
            Vec::new()
        }
    };

    let content_fut = state.runner.one_shot(req);
    let suggestions_fut =
        generate_suggestions(&state, &projects, &msgs_text, &pane_text, &day_str);
    let (content_res, suggestions) = tokio::join!(content_fut, suggestions_fut);

    let res = content_res?;
    let content = if res.text.trim().is_empty() {
        NO_ACTIVITY_RU.to_string()
    } else {
        res.text.trim().to_string()
    };

    let suggestions_value =
        serde_json::to_value(&suggestions).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));

    let report =
        daily_reports::upsert(&state.db, &day_str, &content, source, &suggestions_value).await?;
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

    /// Mock CLI, который эхо-печатает первую строку своего промпта (из argv/stdin
    /// сюда не пробросишь — поэтому проверяем оверрайд иначе: см. ниже).
    /// Для проверки оверрайда достаточно убедиться, что app_settings::set влияет
    /// на эффективный промпт через get(...). Сам факт подстановки тестируем на
    /// уровне app_settings + filter-логики.
    #[tokio::test]
    async fn report_prompt_override_takes_effect_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_summary_script());
        let state = make_state(cli).await;

        // Без оверрайда — эффективный промпт это дефолт.
        let effective = crate::db::repo::app_settings::get(&state.db, PROMPT_KEY_REPORT)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| REPORT_META_PROMPT.to_string());
        assert_eq!(effective, REPORT_META_PROMPT);

        // Пустой оверрайд игнорируется (фолбэк на дефолт).
        crate::db::repo::app_settings::set(&state.db, PROMPT_KEY_REPORT, "   ")
            .await
            .unwrap();
        let effective_blank = crate::db::repo::app_settings::get(&state.db, PROMPT_KEY_REPORT)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| REPORT_META_PROMPT.to_string());
        assert_eq!(effective_blank, REPORT_META_PROMPT);

        // Непустой оверрайд побеждает.
        let custom = "Кастомный промпт отчёта";
        crate::db::repo::app_settings::set(&state.db, PROMPT_KEY_REPORT, custom)
            .await
            .unwrap();
        let effective_custom = crate::db::repo::app_settings::get(&state.db, PROMPT_KEY_REPORT)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| REPORT_META_PROMPT.to_string());
        assert_eq!(effective_custom, custom);
        assert_eq!(PROMPT_KEY_SUGGEST, "daily_report.suggest_prompt");
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
