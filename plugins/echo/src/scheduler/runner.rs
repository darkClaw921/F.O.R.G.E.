//! Исполнитель одного autonomous-run'а.
//!
//! `run_task` — async-функция, инкапсулирующая жизненный цикл одного
//! запуска автономной задачи:
//!
//! 1. Создать запись `task_runs` со `status='running'` через
//!    [`autonomous::insert_run`]. Получить `run_id`.
//! 2. Собрать prompt:
//!    - Получить/создать служебную conversation `__autonomous__/<task_id>`.
//!    - Вызвать [`prompt_builder::build`] с `prompt_template` задачи в
//!      качестве user_text. Контекст (capture-pane, memories) подмешивается
//!      по дефолтным [`CtxOpts`] с учётом `task.project_id`.
//! 3. Запустить `ClaudeRunner::one_shot` с `model = task.model`.
//! 4. Записать assistant-message в служебную conversation
//!    (`messages::insert` с role=assistant + usage).
//! 5. Финализировать `task_runs`: `finish_run(status, tokens, message_id, error?)`.
//! 6. Обновить `next_run_at = now + interval_seconds`. Это делается ВСЕГДА
//!    (даже при ошибке) — чтобы задача не залипла в hot-loop при бесконечной
//!    ошибке Claude CLI.
//! 7. Записать минутный bucket в `token_stats` (`stats::add_tokens`).
//! 8. Бродкастнуть `ServerMsg::AutonomousTaskEvent`.
//!
//! ## Error handling
//!
//! Любая ошибка на шагах 2-4 → `finish_run(status="error", error=Some(...))`
//! + всё равно сдвигаем `next_run_at`. Возвращаемое значение
//! `anyhow::Result<()>` нужно scheduler-loop'у только для лога — на работу
//! планировщика оно не влияет.
//!
//! ## Hard-block system-actions
//!
//! В Phase 5 в action-executor'е появится «autonomous-context»-флаг,
//! отвергающий опасные actions из автономного режима. В Phase 4 мы только
//! помечаем event как `autonomous_task_event` — фронтенд/executor сами
//! решают.

use std::sync::Arc;

use echo_host_api::HostApi;

use crate::actions::{self, Action};
use crate::claude::prompt_builder::{self, CtxOpts};
use crate::claude::RunRequest;
use crate::db::repo::{autonomous, chats, messages, stats};
use crate::db::repo::autonomous::{AutonomousTask, TaskPatch};
use crate::state::{EchoState, ServerEvent};
use crate::ws::protocol::{NotificationLevel, ServerMsg};

/// id служебной conversation для autonomous-задачи. Детерминированный.
pub fn autonomous_conversation_id(task_id: &str) -> String {
    format!("__autonomous__/{task_id}")
}

/// Возвращает unix-ts начала UTC-дня для произвольного момента (down-round
/// до 00:00:00 UTC). Используется autonomous cap'ом.
pub fn utc_today_start(now_unix: i64) -> i64 {
    let seconds_per_day = 86_400_i64;
    // Целочисленное деление + умножение → начало суток. Работает и для
    // отрицательных значений (за пределами 1970).
    (now_unix / seconds_per_day) * seconds_per_day
}

/// Гарантирует существование служебной conversation для задачи. Идемпотентно.
async fn ensure_autonomous_conversation(
    state: &Arc<EchoState>,
    task: &AutonomousTask,
) -> anyhow::Result<String> {
    let conv_id = autonomous_conversation_id(&task.id);
    if let Some(_existing) = chats::get(&state.db, &conv_id).await? {
        return Ok(conv_id);
    }
    let title = format!("[auto] {}", task.name);
    chats::create_with_id(
        &state.db,
        &conv_id,
        &title,
        task.project_id.as_deref(),
        &task.model,
    )
    .await?;
    Ok(conv_id)
}

/// Полный цикл выполнения одной автономной задачи. См. модульный
/// doc-комментарий для подробностей.
///
/// Возвращает `Ok(())` даже при логических ошибках выполнения — они
/// фиксируются в `task_runs.status='error'` и `task_runs.error`. Внешняя
/// `Err` возникает только при сбое БД, который сделает невозможным даже
/// запись об ошибке (например, SQLite файл удалён).
pub async fn run_task(
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
    task: AutonomousTask,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let next_run_at = now + task.interval_seconds.max(1);

    // Phase 6 — autonomous daily token cap. Если cap > 0 и сумма tokens_in +
    // tokens_out по всем task_runs за сегодняшний UTC-день уже >= cap, мы
    // НЕ запускаем эту задачу: помечаем enabled=false и шлём notification.
    // Логика идемпотентна — следующий tick найдёт enabled=0 и пропустит.
    let cap = state.config.autonomous_max_tokens_per_day;
    if cap > 0 {
        let today_start = utc_today_start(now);
        match autonomous::sum_tokens_since(&state.db, today_start).await {
            Ok(used) if used >= cap => {
                tracing::warn!(
                    target: "forge_echo",
                    task_id = %task.id,
                    used,
                    cap,
                    "autonomous: daily token cap reached, disabling task"
                );
                // Отключаем задачу. Игнорируем ошибку — пусть планировщик
                // повторит при следующем tick'е, главное — НЕ запустить
                // её прямо сейчас.
                let _ = autonomous::update_task(
                    &state.db,
                    &task.id,
                    TaskPatch {
                        enabled: Some(false),
                        ..Default::default()
                    },
                )
                .await;
                let body = format!(
                    "Daily token cap reached ({used} >= {cap}). Task \"{name}\" disabled — re-enable manually after reviewing usage.",
                    name = task.name,
                );
                let _ = state.broadcast.send(ServerEvent::broadcast(
                    ServerMsg::Notification {
                        level: NotificationLevel::Warn,
                        title: "Autonomous tasks paused".into(),
                        body,
                    },
                ));
                let _ = state.broadcast.send(ServerEvent::broadcast(
                    ServerMsg::AutonomousTaskEvent {
                        task_id: task.id.clone(),
                        run_id: String::new(),
                        status: "disabled_by_cap".into(),
                        message_preview: Some(format!("Daily cap reached: {used}/{cap}")),
                    },
                ));
                return Ok(());
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    target: "forge_echo",
                    task_id = %task.id,
                    error = %e,
                    "autonomous: failed to read daily token sum; proceeding without cap check"
                );
            }
        }
    }

    // (1) insert task_run (status="running").
    let run = autonomous::insert_run(&state.db, &task.id, now).await?;
    let run_id = run.id.clone();

    // Сразу сдвигаем next_run_at вперёд, чтобы следующий tick НЕ счёл эту
    // задачу due пока run исполняется (даже если interval < длительности run).
    // В случае ошибки ниже мы НЕ перетираем next_run_at назад — задача всё
    // равно пойдёт в следующий цикл.
    if let Err(e) = autonomous::set_next_run(&state.db, &task.id, next_run_at).await {
        tracing::warn!(task_id = %task.id, error = %e, "set_next_run failed (pre-run)");
    }

    // Бродкаст «running».
    let _ = state.broadcast.send(ServerEvent::broadcast(
        ServerMsg::AutonomousTaskEvent {
            task_id: task.id.clone(),
            run_id: run_id.clone(),
            status: "running".into(),
            message_preview: None,
        },
    ));

    // (2) Conversation + prompt.
    let conv_id = match ensure_autonomous_conversation(&state, &task).await {
        Ok(id) => id,
        Err(e) => {
            return finish_with_error(&state, &task, &run_id, &format!("conversation: {e}"))
                .await;
        }
    };

    let opts = CtxOpts {
        include_pane_capture: true,
        project_id: task.project_id.clone(),
        include_memories: true,
        capture_lines: 200,
        session_filter: None,
    };

    let prompt = match prompt_builder::build(
        &task.prompt_template,
        &opts,
        host.as_ref(),
        &state.db,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return finish_with_error(&state, &task, &run_id, &format!("prompt: {e}"))
                .await;
        }
    };

    // (3) one_shot.
    let req = RunRequest {
        prompt,
        model: Some(task.model.clone()),
        system: None,
        run_id: format!("autonomous:{run_id}"),
    };
    let result = match state.runner.one_shot(req).await {
        Ok(r) => r,
        Err(e) => {
            return finish_with_error(&state, &task, &run_id, &format!("claude: {e}")).await;
        }
    };

    // (4) Сохранить assistant-message.
    let assistant_msg = match messages::insert(
        &state.db,
        &conv_id,
        "assistant",
        &result.text,
        None,
        None,
        result.usage.input_tokens as i64,
        result.usage.output_tokens as i64,
        result.usage.cache_creation_input_tokens as i64,
        result.usage.cache_read_input_tokens as i64,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            return finish_with_error(&state, &task, &run_id, &format!("db: {e}")).await;
        }
    };
    let _ = chats::touch_updated(&state.db, &conv_id).await;

    // (5) finish_run (success).
    if let Err(e) = autonomous::finish_run(
        &state.db,
        &run_id,
        "success",
        Some(&assistant_msg.id),
        result.usage.input_tokens as i64,
        result.usage.output_tokens as i64,
        None,
    )
    .await
    {
        tracing::warn!(task_id = %task.id, error = %e, "finish_run(success) failed");
    }

    // (6) token_stats minute-bucket.
    let now_for_stats = chrono::Utc::now().timestamp();
    if let Err(e) = stats::add_tokens(
        &state.db,
        now_for_stats,
        result.usage.input_tokens as i64,
        result.usage.output_tokens as i64,
        result.usage.cache_creation_input_tokens as i64,
        result.usage.cache_read_input_tokens as i64,
    )
    .await
    {
        tracing::warn!(task_id = %task.id, error = %e, "stats::add_tokens failed");
    }

    // (7) Broadcast success event.
    let preview = preview_text(&result.text, 200);
    let _ = state.broadcast.send(ServerEvent::broadcast(
        ServerMsg::AutonomousTaskEvent {
            task_id: task.id.clone(),
            run_id: run_id.clone(),
            status: "success".into(),
            message_preview: Some(preview),
        },
    ));

    // Phase 5b — извлечь actions из ответа. В autonomous-контексте
    // System-actions сразу отсеиваем; Prompt-actions сохраняем в registry
    // (вдруг пользователь захочет нажать кнопку при ручном открытии чата).
    let parsed = actions::parser::extract(&result.text);
    if !parsed.is_empty() {
        let filtered: Vec<Action> = parsed
            .into_iter()
            .filter(|a| match a {
                Action::Prompt { .. } => true,
                Action::System { id, name, .. } => {
                    tracing::warn!(
                        action_id = %id,
                        name = %name.as_str(),
                        task_id = %task.id,
                        "autonomous run: system action stripped (hard-reject by policy)"
                    );
                    false
                }
            })
            .collect();
        if !filtered.is_empty() {
            let descriptors = state.register_actions(&assistant_msg.id, filtered).await;
            let buttons = ServerMsg::ActionButtons {
                message_id: assistant_msg.id.clone(),
                actions: descriptors,
            };
            let _ = state
                .broadcast
                .send(ServerEvent::to_conversation(conv_id.clone(), buttons));
        }
    }

    tracing::info!(
        task_id = %task.id,
        run_id,
        tokens_in = result.usage.input_tokens,
        tokens_out = result.usage.output_tokens,
        "autonomous task completed"
    );
    Ok(())
}

/// Финализация при ошибке: пометить run как error, бродкастнуть event,
/// вернуть Ok (ошибка задачи не считается фатальной для scheduler'а).
async fn finish_with_error(
    state: &Arc<EchoState>,
    task: &AutonomousTask,
    run_id: &str,
    error: &str,
) -> anyhow::Result<()> {
    tracing::warn!(task_id = %task.id, run_id, error, "autonomous task failed");
    if let Err(e) = autonomous::finish_run(
        &state.db,
        run_id,
        "error",
        None,
        0,
        0,
        Some(error),
    )
    .await
    {
        // Это уже серьёзнее — БД сломана. Бросаем наверх.
        return Err(anyhow::anyhow!(
            "finish_run(error) failed: {e} (original error: {error})"
        ));
    }
    let _ = state.broadcast.send(ServerEvent::broadcast(
        ServerMsg::AutonomousTaskEvent {
            task_id: task.id.clone(),
            run_id: run_id.to_string(),
            status: "error".into(),
            message_preview: Some(preview_text(error, 200)),
        },
    ));
    Ok(())
}

/// Возвращает первые `n` символов строки, обрезает с учётом UTF-8 границ.
fn preview_text(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let truncated: String = s.chars().take(n).collect();
    format!("{truncated}…")
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

    fn write_mock(dir: &tempfile::TempDir, script: &str) -> PathBuf {
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
        let state = Arc::new(EchoState::new(Arc::new(db), runner));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host).ok();
        state
    }

    async fn make_state_with_cap(cli: PathBuf, cap: u64) -> Arc<EchoState> {
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let cfg = crate::config::EchoConfig {
            autonomous_max_tokens_per_day: cap,
            ..crate::config::EchoConfig::default()
        };
        let state = Arc::new(EchoState::new_with_config(Arc::new(db), runner, cfg));
        let host: Arc<dyn HostApi> = Arc::new(StubHost);
        state.host.set(host).ok();
        state
    }

    #[tokio::test]
    async fn run_task_success_writes_assistant_msg_and_finishes_run() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi "}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"bot"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":5,"output_tokens":2}}'
"#;
        let cli = write_mock(&dir, script);
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let task = autonomous::create_task(&state.db, "tt", "say hi", 60, "sonnet-4", None)
            .await
            .unwrap();

        let mut rx = state.broadcast.subscribe();
        run_task(state.clone(), host, task.clone()).await.unwrap();

        // Run record success
        let runs = autonomous::list_runs(&state.db, &task.id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
        assert!(runs[0].result_message_id.is_some());
        assert_eq!(runs[0].tokens_in, 5);
        assert_eq!(runs[0].tokens_out, 2);

        // Conversation создан, assistant сообщение там.
        let conv = autonomous_conversation_id(&task.id);
        let chat = chats::get(&state.db, &conv).await.unwrap().unwrap();
        assert!(chat.title.contains("tt"));
        let msgs = messages::list_by_session(&state.db, &conv, 10, None).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content, "Hi bot");

        // token_stats записан.
        let now = chrono::Utc::now().timestamp();
        let bucket = now / 60;
        let s = stats::range(&state.db, bucket - 1, bucket + 1).await.unwrap();
        let total_in: i64 = s.iter().map(|b| b.tokens_in).sum();
        assert!(total_in >= 5, "stats bucket must include tokens (got {total_in})");

        // Broadcast events: первое — running, второе — success.
        let mut got_running = false;
        let mut got_success = false;
        for _ in 0..2 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(ev)) => match ev.msg {
                    ServerMsg::AutonomousTaskEvent { status, .. } if status == "running" => {
                        got_running = true;
                    }
                    ServerMsg::AutonomousTaskEvent { status, .. } if status == "success" => {
                        got_success = true;
                    }
                    _ => {}
                },
                _ => break,
            }
        }
        assert!(got_running, "expected running event");
        assert!(got_success, "expected success event");
    }

    #[tokio::test]
    async fn run_task_advances_next_run_at_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock(&dir, script);
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let task = autonomous::create_task(&state.db, "n", "p", 30, "m", None)
            .await
            .unwrap();
        // Принудительно ставим next_run_at в прошлое.
        let now = chrono::Utc::now().timestamp();
        autonomous::set_next_run(&state.db, &task.id, now - 100).await.unwrap();

        run_task(state.clone(), host, task.clone()).await.unwrap();
        let t = autonomous::get_task(&state.db, &task.id).await.unwrap().unwrap();
        assert!(t.next_run_at.unwrap() > now, "must be advanced");
    }

    #[tokio::test]
    async fn run_task_records_error_when_cli_missing() {
        // CLI отсутствует — ClaudeRunner::one_shot вернёт Err.
        let state = make_state(PathBuf::from("/totally/missing/cli")).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let task = autonomous::create_task(&state.db, "err", "p", 60, "m", None)
            .await
            .unwrap();

        run_task(state.clone(), host, task.clone()).await.unwrap();
        let runs = autonomous::list_runs(&state.db, &task.id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "error");
        assert!(runs[0].error.as_ref().unwrap().contains("claude"));
        // next_run_at должен быть сдвинут (защита от hot-loop).
        let t = autonomous::get_task(&state.db, &task.id).await.unwrap().unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!(t.next_run_at.unwrap() >= now);
    }

    #[tokio::test]
    async fn autonomous_conversation_is_reused_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock(&dir, script);
        let state = make_state(cli).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let task = autonomous::create_task(&state.db, "x", "p", 60, "m", None)
            .await
            .unwrap();
        run_task(state.clone(), host.clone(), task.clone()).await.unwrap();
        run_task(state.clone(), host, task.clone()).await.unwrap();

        let conv = autonomous_conversation_id(&task.id);
        let msgs = messages::list_by_session(&state.db, &conv, 10, None).await.unwrap();
        assert_eq!(msgs.len(), 2, "оба run'а пишут assistant-msg в одну conversation");
    }

    #[tokio::test]
    async fn run_task_skipped_and_disabled_when_daily_cap_reached() {
        // Сценарий: cap=100 токенов, уже потрачено 150 — задача должна
        // быть отключена и пользователю уйти warn-нотификация. CLI здесь
        // не вызывается (cap-check перед any work).
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock(&dir, script);
        let state = make_state_with_cap(cli, 100).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        // Создаём фейковый run с уже потраченными токенами «сегодня».
        let task = autonomous::create_task(&state.db, "t", "p", 60, "m", None)
            .await
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        let r = autonomous::insert_run(&state.db, &task.id, now)
            .await
            .unwrap();
        autonomous::finish_run(&state.db, &r.id, "success", None, 100, 50, None)
            .await
            .unwrap();
        assert_eq!(
            autonomous::sum_tokens_since(&state.db, utc_today_start(now))
                .await
                .unwrap(),
            150
        );

        // Ставим next_run_at в прошлое для реализма и запускаем.
        autonomous::set_next_run(&state.db, &task.id, now - 5)
            .await
            .unwrap();

        let mut rx = state.broadcast.subscribe();
        run_task(state.clone(), host, task.clone()).await.unwrap();

        // Задача должна стать enabled=false; новых runs не появилось.
        let updated = autonomous::get_task(&state.db, &task.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!updated.enabled, "task must be disabled when cap reached");

        let runs = autonomous::list_runs(&state.db, &task.id, 10)
            .await
            .unwrap();
        // Только тот run, который мы вручную создали — никаких новых.
        assert_eq!(runs.len(), 1);

        // Должно прийти как минимум одно Notification (warn) и один
        // AutonomousTaskEvent {status: "disabled_by_cap"}.
        let mut got_notification = false;
        let mut got_disabled_event = false;
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(ev)) => match ev.msg {
                    ServerMsg::Notification { level, title, .. }
                        if matches!(level, NotificationLevel::Warn)
                            && title.contains("Autonomous") =>
                    {
                        got_notification = true;
                    }
                    ServerMsg::AutonomousTaskEvent { status, .. }
                        if status == "disabled_by_cap" =>
                    {
                        got_disabled_event = true;
                    }
                    _ => {}
                },
                _ => break,
            }
        }
        assert!(got_notification, "expected warn notification");
        assert!(got_disabled_event, "expected disabled_by_cap event");
    }

    #[tokio::test]
    async fn run_task_cap_disabled_when_zero() {
        // cap=0 — лимит выключен, задача исполняется как обычно.
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":50000,"output_tokens":50000}}'
"#;
        let cli = write_mock(&dir, script);
        let state = make_state_with_cap(cli, 0).await;
        let host: Arc<dyn HostApi> = Arc::new(StubHost);

        let task = autonomous::create_task(&state.db, "tt", "p", 60, "m", None)
            .await
            .unwrap();
        run_task(state.clone(), host, task.clone()).await.unwrap();

        let runs = autonomous::list_runs(&state.db, &task.id, 10)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "success");
    }

    #[test]
    fn utc_today_start_floors_to_midnight() {
        // 2026-05-17 13:45:00 UTC ≈ 1779631500
        let ts = 1_779_631_500_i64;
        let start = utc_today_start(ts);
        // 2026-05-17 00:00:00 UTC = 1779580800
        assert_eq!(start, 1_779_580_800);
        // И ts > start, ts < start + 86400.
        assert!(ts >= start);
        assert!(ts < start + 86_400);
    }

    #[test]
    fn preview_truncates_long_text() {
        let s = "x".repeat(500);
        let p = preview_text(&s, 200);
        assert!(p.ends_with('…'));
        assert_eq!(p.chars().count(), 201);
    }

    #[test]
    fn preview_does_not_truncate_short_text() {
        assert_eq!(preview_text("short", 200), "short");
    }
}
