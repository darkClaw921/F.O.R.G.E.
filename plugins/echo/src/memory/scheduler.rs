//! Memory rollover scheduler (hourly).
//!
//! Фоновый loop, который раз в час смотрит на текущий UTC-день и
//! сравнивает его с последним обработанным днём. При смене дня
//! генерирует `global_day` summary за вчерашний день (через
//! [`super::summarize_day`] с `project_id=None`).
//!
//! После Phase 4 (`remove-projects-concept`) per-project rollover удалён:
//! HostApi больше не выдаёт список проектов, поэтому scheduler не имеет
//! возможности их перечислить. `summarize_day(project_id=Some(..))` и
//! [`super::summarize_project`] остаются доступными через REST-эндпоинты
//! Echo (caller передаёт project_id явно), но автоматический per-project
//! rollover больше не вызывается.
//!
//! Маркер «последний обработанный день» сохраняется в `memories` со scope
//! `global_day`, project_id=NULL, day=`__last_rollover__` (специальный sentinel
//! который не конфликтует с реальным `YYYY-MM-DD`). При старте `spawn`
//! читаем этот маркер, чтобы не перезапускать суммаризацию за уже
//! обработанный день.
//!
//! ## Тестирование
//!
//! `tick_once(state, host, force_yesterday)` вынесен в `pub(crate)` — даёт
//! детерминированный hook для unit-тестов: вызвать с `force_yesterday =
//! Some(date)` чтобы пропустить date-сравнение.

use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use tokio::task::JoinHandle;

use echo_host_api::HostApi;

use crate::db::repo::memories;
use crate::state::EchoState;

/// Период опроса дневного rollover'а. 1 час — компромисс между точностью
/// (хочется суммаризовать сразу после полуночи) и нагрузкой (одна
/// query-операция на час).
pub const TICK_INTERVAL: Duration = Duration::from_secs(3600);

/// Sentinel-day, под которым хранится маркер «последний обработанный день».
/// Не пересекается с настоящими `YYYY-MM-DD` (формат явно различается).
const MARKER_DAY: &str = "__last_rollover__";

/// Источник для маркера — позволяет фильтровать его из обычного UI-листинга
/// (фронтенд может пропускать source="_marker_").
const MARKER_SOURCE: &str = "_marker_";

/// Спавнит rollover-loop. Возвращает `JoinHandle`, который вызывающий
/// может abort'нуть для graceful shutdown.
pub fn spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<()> {
    tracing::info!(
        tick_secs = TICK_INTERVAL.as_secs(),
        "Echo memory rollover scheduler started"
    );
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            if let Err(e) = tick_once(state.clone(), host.clone(), None).await {
                tracing::warn!(error = %e, "memory::scheduler: tick_once failed");
            }
        }
    })
}

/// Одна итерация. Если `force_yesterday` задан — пропускаем сравнение с
/// маркером и сразу запускаем rollover за указанную дату (тестовый hook).
/// Иначе — сравниваем `Utc::now().date_naive()` с маркером и при отличии
/// rollover'им за вчера и обновляем маркер.
pub async fn tick_once(
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
    force_yesterday: Option<NaiveDate>,
) -> anyhow::Result<()> {
    let today = Utc::now().date_naive();
    let last = read_marker(&state).await;

    let yesterday = if let Some(forced) = force_yesterday {
        forced
    } else {
        // Если уже обрабатывали сегодня — выходим.
        if last == Some(today) {
            tracing::trace!("memory::scheduler: already processed today, skip");
            return Ok(());
        }
        today - chrono::Duration::days(1)
    };

    tracing::info!(
        ?last,
        %today,
        %yesterday,
        "memory::scheduler: rollover triggered"
    );

    // Global day summary. Per-project rollover удалён вместе с
    // концепцией проектов (Phase 4) — HostApi больше не предоставляет
    // список. Caller, которому нужен project_day summary, должен вызвать
    // `summarize_day(project_id=Some(..))` напрямую через REST.
    if let Err(e) = super::summarize_day(state.clone(), host.clone(), yesterday, None).await {
        tracing::warn!(error = %e, %yesterday, "memory::scheduler: global summarize_day failed");
    }

    // Обновить маркер на today.
    write_marker(&state, today).await?;
    Ok(())
}

/// Читает маркер «последний обработанный день» из таблицы memories.
async fn read_marker(state: &Arc<EchoState>) -> Option<NaiveDate> {
    let res = memories::list(
        &state.db,
        Some(memories::MemoryScope::GlobalDay),
        None,
        Some(MARKER_DAY),
    )
    .await
    .ok()?;
    let m = res.into_iter().find(|m| m.source == MARKER_SOURCE)?;
    NaiveDate::parse_from_str(m.content.trim(), "%Y-%m-%d").ok()
}

/// Перезаписывает маркер. content = `YYYY-MM-DD` today.
async fn write_marker(state: &Arc<EchoState>, today: NaiveDate) -> anyhow::Result<()> {
    let content = today.format("%Y-%m-%d").to_string();
    memories::upsert(
        &state.db,
        memories::MemoryScope::GlobalDay,
        None,
        Some(MARKER_DAY),
        &content,
        MARKER_SOURCE,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeRunner;
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

    fn mock_script() -> &'static str {
        r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Auto summary"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":2,"output_tokens":2}}'
"#
    }

    async fn make_state(cli: PathBuf) -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        Arc::new(EchoState::new(Arc::new(db), runner))
    }

    #[tokio::test]
    async fn marker_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_script());
        let state = make_state(cli).await;
        assert_eq!(read_marker(&state).await, None);
        let today = NaiveDate::from_ymd_opt(2026, 5, 17).unwrap();
        write_marker(&state, today).await.unwrap();
        assert_eq!(read_marker(&state).await, Some(today));
    }

    #[tokio::test]
    async fn tick_runs_global_summary_and_updates_marker() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let yesterday = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        tick_once(state.clone(), host, Some(yesterday)).await.unwrap();

        // Global day memory создан.
        let g = memories::list(
            &state.db,
            Some(memories::MemoryScope::GlobalDay),
            None,
            Some("2026-05-16"),
        )
        .await
        .unwrap();
        assert_eq!(g.len(), 1);
        // Marker присутствует.
        assert!(read_marker(&state).await.is_some());
    }

    #[tokio::test]
    async fn second_tick_same_day_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cli = write_mock_cli(&dir, mock_script());
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        // Первый tick без force_yesterday: маркер ставится в today, rollover
        // отрабатывает для вчера.
        tick_once(state.clone(), host.clone(), None).await.unwrap();
        let after_first = read_marker(&state).await;
        assert!(after_first.is_some());

        // Создаём «сегодняшний» global_day, чтобы заметить, был ли повторный
        // вызов summarize_day.
        let _before_count = memories::list(
            &state.db,
            Some(memories::MemoryScope::GlobalDay),
            None,
            None,
        )
        .await
        .unwrap()
        .len();

        // Второй tick — маркер == today, должен быть no-op (не пересоздаём
        // global_day за вчера).
        tick_once(state.clone(), host, None).await.unwrap();
        let after_second = read_marker(&state).await;
        assert_eq!(after_first, after_second, "marker must not change");
    }
}
