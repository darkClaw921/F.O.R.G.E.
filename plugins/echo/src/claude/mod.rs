//! Claude CLI integration.
//!
//! Sub-модули:
//! - [`events`] — NDJSON-парсер `claude -p --output-format stream-json`.
//! - [`prompt_builder`] — сборка prompt'а с capture-pane + memories.
//!
//! Главный фасад — [`ClaudeRunner`].
//!
//! ## Архитектура запуска
//!
//! `ClaudeRunner` спавнит дочерний процесс `claude -p --output-format
//! stream-json --include-partial-messages --verbose [--model <m>] <prompt>` и
//! построчно читает его stdout, парся каждую строку в [`events::ClaudeEvent`].
//!
//! - Параллелизм ограничен `Semaphore` (по умолчанию 4 одновременных run'а).
//!   Слишком много параллельных Claude CLI убивает квоту и плодит зомби.
//! - Каждый активный run регистрируется в `running: HashMap<RunId, AbortHandle>`,
//!   что даёт `cancel(run_id)` без зависания.
//! - Не падаем, если `cli_path` не существует на момент `new()` — только
//!   warn-log. Это позволяет `EchoState::init` собирать runner даже когда
//!   Claude CLI не установлен (для healthz/тестов).

pub mod events;
pub mod prompt_builder;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::task::AbortHandle;

pub use events::{ClaudeEvent, Usage};

/// Уникальный идентификатор run'а. Совпадает по типу с `String` для
/// тривиальной сериализации в WS-протокол.
pub type RunId = String;

/// Запрос на запуск Claude CLI.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Полный prompt (уже собранный prompt_builder'ом).
    pub prompt: String,
    /// Модель Claude (`--model`). `None` → дефолт CLI.
    pub model: Option<String>,
    /// Дополнительный system-prompt (`--append-system-prompt`).
    pub system: Option<String>,
    /// ID run'а — генерируется callee. Используется для cancel.
    pub run_id: RunId,
}

impl RunRequest {
    /// Утилита для тестов и быстрых запусков: только prompt + новый UUID run_id.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            system: None,
            run_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// Результат `one_shot` — собранный текст + итоговый usage + сырой json для аудита.
#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub text: String,
    pub usage: Usage,
    pub raw: serde_json::Value,
}

/// Фасад над Claude CLI: spawn, stream, cancel.
///
/// Cheap-clonable через `Arc`. Один процесс devforge → один runner; внутри
/// runner может быть несколько одновременных stream'ов до значения semaphore.
pub struct ClaudeRunner {
    cli_path: PathBuf,
    semaphore: Arc<Semaphore>,
    running: Arc<Mutex<HashMap<RunId, AbortHandle>>>,
}

impl ClaudeRunner {
    /// Создаёт runner. **Не падает**, если `cli_path` не существует — только
    /// warn-log. Это намеренно: `init` плагина не должен падать в окружении
    /// без Claude CLI (тесты, dev-машины без CLI).
    pub fn new(cli_path: PathBuf, max_parallel: usize) -> Self {
        if !cli_path.exists() {
            tracing::warn!(
                cli = %cli_path.display(),
                "ClaudeRunner: cli binary not found at startup — run requests will fail at exec time"
            );
        }
        let permits = max_parallel.max(1);
        Self {
            cli_path,
            semaphore: Arc::new(Semaphore::new(permits)),
            running: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Сколько свободных permit'ов сейчас (для тестов и метрик).
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Сколько активных run'ов сейчас (для тестов и /api/echo/runs).
    pub async fn active_runs(&self) -> usize {
        self.running.lock().await.len()
    }

    /// Запускает Claude CLI и возвращает receiver-сторону канала с
    /// событиями [`ClaudeEvent`]. Канал закрывается, когда CLI завершился
    /// (EOF на stdout) или run был отменён.
    ///
    /// Не блокирует caller'а на permit'е — permit берётся уже внутри
    /// spawned-таски, чтобы caller сразу получил `Receiver` и подписался на
    /// первые события.
    pub async fn stream(&self, req: RunRequest) -> mpsc::Receiver<ClaudeEvent> {
        let (tx, rx) = mpsc::channel::<ClaudeEvent>(64);
        let cli_path = self.cli_path.clone();
        let semaphore = self.semaphore.clone();
        let running = self.running.clone();
        let run_id = req.run_id.clone();
        let run_id_for_map = run_id.clone();

        let handle = tokio::spawn(async move {
            // Permit удерживаем на всё время run'а — Semaphore ограничивает
            // ОДНОВРЕМЕННЫЕ запуски, что и требовалось.
            let _permit = match semaphore.acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "semaphore closed unexpectedly");
                    let _ = tx
                        .send(ClaudeEvent::Error {
                            message: "semaphore closed".into(),
                        })
                        .await;
                    return;
                }
            };

            let mut cmd = Command::new(&cli_path);
            cmd.arg("-p")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--include-partial-messages")
                .arg("--verbose");

            if let Some(model) = &req.model {
                cmd.arg("--model").arg(model);
            }
            if let Some(sys) = &req.system {
                cmd.arg("--append-system-prompt").arg(sys);
            }
            // Prompt — последний positional аргумент.
            cmd.arg(&req.prompt);

            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        cli = %cli_path.display(),
                        "failed to spawn claude CLI"
                    );
                    let _ = tx
                        .send(ClaudeEvent::Error {
                            message: format!("spawn failed: {e}"),
                        })
                        .await;
                    return;
                }
            };

            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    tracing::error!("claude CLI: stdout pipe not captured");
                    let _ = tx
                        .send(ClaudeEvent::Error {
                            message: "stdout pipe missing".into(),
                        })
                        .await;
                    let _ = child.kill().await;
                    return;
                }
            };

            // Stderr читаем в отдельной таске для логирования (не emit'им как
            // ClaudeEvent::Error — там бывают noisy warnings).
            if let Some(stderr) = child.stderr.take() {
                let run_id_for_log = run_id.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(run_id = %run_id_for_log, stderr = %line, "claude stderr");
                    }
                });
            }

            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if let Some(ev) = events::parse_line(&line) {
                            if tx.send(ev).await.is_err() {
                                // Потребитель ушёл — убиваем процесс.
                                tracing::debug!(run_id = %run_id, "receiver dropped, killing child");
                                let _ = child.kill().await;
                                break;
                            }
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(e) => {
                        tracing::warn!(error = %e, "claude stdout read error");
                        break;
                    }
                }
            }

            // Дождаться завершения процесса (или kill); игнорируем код выхода —
            // для callee важны только события.
            let _ = child.wait().await;

            // Снимаем регистрацию.
            running.lock().await.remove(&req.run_id);
        });

        let abort_handle = handle.abort_handle();
        self.running
            .lock()
            .await
            .insert(run_id_for_map, abort_handle);

        rx
    }

    /// Запускает CLI и собирает все события до завершения. Возвращает
    /// агрегированный текст и финальный usage.
    pub async fn one_shot(&self, req: RunRequest) -> anyhow::Result<RunResult> {
        let mut rx = self.stream(req).await;
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut raw = serde_json::Value::Null;
        let mut last_error: Option<String> = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                ClaudeEvent::TextDelta { text: t } => text.push_str(&t),
                ClaudeEvent::Thinking { .. } => {}
                ClaudeEvent::ToolUse { .. } => {}
                ClaudeEvent::Result { usage: u, raw_json } => {
                    usage = u;
                    raw = raw_json;
                }
                ClaudeEvent::Error { message } => {
                    last_error = Some(message);
                }
            }
        }
        if let Some(msg) = last_error {
            // Если был ошибочный event, но ничего полезного не собрали — Err.
            if text.is_empty() {
                anyhow::bail!("claude run error: {msg}");
            }
        }
        Ok(RunResult { text, usage, raw })
    }

    /// Прерывает run по id. Возвращает `true` если run был найден и aborted.
    pub async fn cancel(&self, run_id: &str) -> bool {
        let mut map = self.running.lock().await;
        if let Some(h) = map.remove(run_id) {
            h.abort();
            tracing::info!(target: "forge_echo", run_id, "ClaudeRunner: cancelled");
            true
        } else {
            false
        }
    }

    /// Phase 6 hardening — abort'нуть все активные run-задачи. Дочерние
    /// процессы Claude CLI убиваются автоматически благодаря `kill_on_drop`
    /// в [`tokio::process::Command`], потому что drop spawned tokio-task'и
    /// приводит к drop'у `child` внутри неё.
    pub async fn shutdown(&self) {
        let mut map = self.running.lock().await;
        let n = map.len();
        for (_id, h) in map.drain() {
            h.abort();
        }
        if n > 0 {
            tracing::info!(target: "forge_echo", killed = n, "ClaudeRunner: shutdown — aborted active runs");
        } else {
            tracing::debug!(target: "forge_echo", "ClaudeRunner: shutdown — no active runs");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_mock_cli(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    async fn new_warns_on_missing_cli_but_does_not_panic() {
        // Не должно паниковать.
        let r = ClaudeRunner::new(PathBuf::from("/does/not/exist"), 2);
        assert_eq!(r.available_permits(), 2);
    }

    #[tokio::test]
    async fn stream_yields_events_from_mock_cli() {
        let dir = tempfile::tempdir().unwrap();
        // Mock CLI: игнорит args, печатает 2 text_delta + result, выходит 0.
        // Игнорим аргументы через "$@" в shebang-free скрипте.
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":10,"output_tokens":2}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4);
        let mut rx = runner
            .stream(RunRequest::new("test"))
            .await;

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 3, "got {events:?}");
        match &events[0] {
            ClaudeEvent::TextDelta { text } => assert_eq!(text, "Hello "),
            other => panic!("unexpected: {other:?}"),
        }
        match &events[2] {
            ClaudeEvent::Result { usage, .. } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_shot_aggregates_text_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"foo"}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"bar"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":7,"output_tokens":3}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = ClaudeRunner::new(cli, 4);
        let res = runner.one_shot(RunRequest::new("hi")).await.unwrap();
        assert_eq!(res.text, "foobar");
        assert_eq!(res.usage.input_tokens, 7);
        assert_eq!(res.usage.output_tokens, 3);
    }

    #[tokio::test]
    async fn cancel_aborts_running_stream() {
        let dir = tempfile::tempdir().unwrap();
        // Mock CLI: спит 30 секунд между чанками — даём время отменить.
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"start"}}'
sleep 30
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let req = RunRequest::new("slow");
        let run_id = req.run_id.clone();
        let mut rx = runner.stream(req).await;

        // Получаем первый чанк.
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("first chunk timed out")
            .expect("first chunk None");
        assert!(matches!(first, ClaudeEvent::TextDelta { .. }));

        // Cancel — должен прервать stream быстро.
        assert!(runner.cancel(&run_id).await, "cancel returned false");

        // Канал должен закрыться (либо новых событий не будет за разумный
        // тайм-аут — мы убиваем процесс через kill_on_drop).
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        // Либо None (закрыт), либо тайм-аут — оба означают, что long sleep не
        // продолжился. Не должны получить второй текстовый чанк.
        match next {
            Ok(None) => { /* ok — закрылся */ }
            Ok(Some(ev)) => match ev {
                ClaudeEvent::Result { .. } => panic!("got Result after cancel"),
                ClaudeEvent::TextDelta { .. } => panic!("got more text after cancel"),
                _ => {}
            },
            Err(_) => { /* тайм-аут — тоже ок, abort произошёл */ }
        }
    }

    #[tokio::test]
    async fn shutdown_aborts_running_streams() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}'
sleep 30
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 4));
        let _rx = runner.stream(RunRequest::new("slow")).await;
        // Дать таске взять permit и зарегистрироваться.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(runner.active_runs().await >= 1);
        runner.shutdown().await;
        // После shutdown — running map пуст.
        assert_eq!(runner.active_runs().await, 0);
    }

    #[tokio::test]
    async fn semaphore_caps_concurrent_streams() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
sleep 2
printf '%s\n' '{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
        let cli = write_mock_cli(&dir, script);
        let runner = Arc::new(ClaudeRunner::new(cli, 2));

        // Запускаем 2 stream'а — оба должны быстро взять permit.
        let _rx1 = runner.stream(RunRequest::new("a")).await;
        let _rx2 = runner.stream(RunRequest::new("b")).await;

        // Дать таскам время взять permit'ы.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // Permit'ов должно быть 0 (все 2 заняты).
        assert_eq!(
            runner.available_permits(),
            0,
            "expected 0 permits after 2 streams started"
        );
    }
}
