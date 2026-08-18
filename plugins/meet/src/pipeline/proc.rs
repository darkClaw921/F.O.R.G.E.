//! Запуск внешних процессов пост-обработки.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # ХВОСТ STDERR ДОХОДИТ ДО ПОЛЬЗОВАТЕЛЯ
//!
//! Не в лог сервера, а в `meeting_events` и в кадр `error`. Разница
//! практическая: ffmpeg на битом WAV пишет «Invalid data found when processing
//! input» последней строкой из сорока, и без этой строки человек видит только
//! «встреча не обработалась» — а дальше ему нечего делать, кроме как писать в
//! поддержку.
//!
//! Именно ХВОСТ, а не начало: у ffmpeg первые строки — это баннер версии и
//! перечисление кодеков, причина всегда в конце.
//!
//! # Зависший процесс УБИВАЕТСЯ
//!
//! `kill_on_drop(true)` плюс явный `kill()` по таймауту. Без этого зависший
//! ffmpeg держал бы слот записи и pipe stderr до конца жизни devforge.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::error::MeetError;

/// Сколько последних строк stderr показывается пользователю.
///
/// Двадцать: меньше — теряется контекст ошибки, больше — в карточку встречи
/// приезжает простыня из прогресса ffmpeg.
pub const STDERR_TAIL_LINES: usize = 20;

/// Провал внешнего процесса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcError {
    /// Машиночитаемый код: `timeout`, `exit_failed`, `metal_unavailable`, …
    pub code: &'static str,
    /// Текст для человека.
    pub message: String,
    /// Последние строки stderr — то, ради чего всё и затевалось.
    pub stderr_tail: String,
}

impl ProcError {
    pub fn new(code: &'static str, message: impl Into<String>, tail: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            stderr_tail: tail.into(),
        }
    }

    /// Полный текст с хвостом — то, что видит пользователь.
    pub fn full_message(&self) -> String {
        if self.stderr_tail.trim().is_empty() {
            self.message.clone()
        } else {
            format!("{}\n{}", self.message, self.stderr_tail)
        }
    }
}

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.full_message())
    }
}

impl From<ProcError> for MeetError {
    fn from(e: ProcError) -> Self {
        // Код Metal — часть лестницы 409: расшифровка не может пойти на CPU,
        // и пользователю нужно ровно это слово, а не «внутренняя ошибка».
        if e.code == "metal_unavailable" {
            return MeetError::DepUnavailable("metal_unavailable");
        }
        MeetError::Internal(e.full_message())
    }
}

/// Последние `n` непустых строк текста.
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(n);
    lines[from..].join("\n")
}

/// Копит хвост stderr, не держа в памяти весь вывод.
///
/// Кольцо, а не `String`: whisper с `-pp` печатает прогресс сотнями строк, и
/// на трёхчасовой встрече полный вывод — это мегабайты, из которых нужны
/// последние двадцать строк.
#[derive(Debug, Default)]
pub struct TailBuffer {
    lines: std::collections::VecDeque<String>,
}

impl TailBuffer {
    pub fn push(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.lines.len() == STDERR_TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.trim_end().to_string());
    }

    pub fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Как поступить со строкой stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineVerdict {
    /// Продолжать.
    Continue,
    /// Убить процесс немедленно с этим кодом и текстом.
    Kill(&'static str, String),
}

/// Запускает процесс, читает его stderr построчно и ждёт не дольше таймаута.
///
/// `watch` вызывается на КАЖДОЙ строке stderr и может потребовать убийства
/// процесса, не дожидаясь его конца. Ради этого функция и существует: Metal-
/// гард обязан снять прогон, который пошёл на CPU, а не досчитать его молча.
pub async fn run_watched<F>(
    bin: &Path,
    args: &[String],
    timeout: Duration,
    cancel: &CancellationToken,
    mut watch: F,
) -> Result<String, ProcError>
where
    F: FnMut(&str) -> LineVerdict,
{
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        // stdin закрыт: ffmpeg без него на неоднозначном вопросе ждёт ответа
        // с клавиатуры и висит до таймаута.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        ProcError::new(
            "spawn_failed",
            format!("{} не запустился: {e}", bin.display()),
            "",
        )
    })?;
    let stderr = child.stderr.take().expect("stderr запрошен piped");
    let mut lines = BufReader::new(stderr).lines();
    let mut buf = TailBuffer::default();

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let killed: Option<ProcError> = loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                break Some(ProcError::new("cancelled", "операция отменена", buf.text()));
            }

            _ = &mut deadline => {
                let _ = child.kill().await;
                break Some(ProcError::new(
                    "timeout",
                    format!("{} не ответил за {:?}", bin.display(), timeout),
                    buf.text(),
                ));
            }

            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    buf.push(&l);
                    if let LineVerdict::Kill(code, message) = watch(&l) {
                        let _ = child.kill().await;
                        break Some(ProcError::new(code, message, buf.text()));
                    }
                }
                // EOF либо ошибка чтения — процесс закончился.
                _ => break None,
            },
        }
    };

    // Ждём сам процесс в любом случае: без этого убитый остался бы зомби.
    let status = child.wait().await;
    if let Some(err) = killed {
        return Err(err);
    }
    match status {
        Ok(s) if s.success() => Ok(buf.text()),
        Ok(s) => Err(ProcError::new(
            "exit_failed",
            format!("{} вышел с кодом {}", bin.display(), s.code().unwrap_or(-1)),
            buf.text(),
        )),
        Err(e) => Err(ProcError::new(
            "wait_failed",
            format!("{} не дождался: {e}", bin.display()),
            buf.text(),
        )),
    }
}

/// Запуск без наблюдения за строками.
pub async fn run(
    bin: &Path,
    args: &[String],
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<String, ProcError> {
    run_watched(bin, args, timeout, cancel, |_| LineVerdict::Continue).await
}

/// Запуск ради stdout (ffprobe).
pub async fn capture_stdout(
    bin: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<String, ProcError> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| {
        ProcError::new("spawn_failed", format!("{} не запустился: {e}", bin.display()), "")
    })?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Ok(Ok(out)) => Err(ProcError::new(
            "exit_failed",
            format!("{} вышел с кодом {}", bin.display(), out.status.code().unwrap_or(-1)),
            tail(&String::from_utf8_lossy(&out.stderr), STDERR_TAIL_LINES),
        )),
        Ok(Err(e)) => Err(ProcError::new(
            "wait_failed",
            format!("{} не дождался: {e}", bin.display()),
            "",
        )),
        Err(_) => Err(ProcError::new(
            "timeout",
            format!("{} не ответил за {timeout:?}", bin.display()),
            "",
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::mocks;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        crate::testing::process_count_guard()
    }

    #[test]
    fn the_tail_keeps_the_end_not_the_beginning() {
        let text: String = (1..=100).map(|i| format!("строка {i}\n")).collect();
        let t = tail(&text, 3);
        assert_eq!(t, "строка 98\nстрока 99\nстрока 100");
        assert_eq!(tail("", 5), "");
        // Пустые строки не съедают место в хвосте.
        assert_eq!(tail("a\n\n\n\nb", 2), "a\nb");
    }

    #[test]
    fn the_tail_buffer_never_grows_past_the_limit() {
        let mut b = TailBuffer::default();
        for i in 0..10_000 {
            b.push(&format!("line {i}"));
        }
        assert_eq!(b.text().lines().count(), STDERR_TAIL_LINES);
        assert!(b.text().ends_with("line 9999"));
    }

    #[tokio::test]
    async fn a_successful_run_returns_its_stderr_tail() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::mock_success(dir.path(), "ok", "", "готово\n").unwrap();
        let out = run(&bin, &[], Duration::from_secs(5), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "готово");
    }

    /// ГЛАВНОЕ: причина падения доезжает до вызывающего, а не теряется.
    #[tokio::test]
    async fn a_failing_run_carries_the_reason_out() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::mock_ffmpeg_failure(dir.path(), "ffmpeg", "Invalid data found").unwrap();
        let err = run(&bin, &[], Duration::from_secs(5), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.code, "exit_failed");
        assert!(err.stderr_tail.ends_with("Invalid data found"), "{}", err.stderr_tail);
        assert!(err.full_message().contains("Invalid data found"));
        // И хвост именно хвост: сорока строк прогресса в нём нет целиком.
        assert!(err.stderr_tail.lines().count() <= STDERR_TAIL_LINES);
    }

    /// Зависший процесс убивается по таймауту, а тест не висит.
    #[tokio::test]
    async fn a_hanging_process_is_killed_by_the_timeout() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::mock_hang(dir.path(), "hang", 120).unwrap();
        let started = std::time::Instant::now();
        let err = run(&bin, &[], Duration::from_millis(200), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.code, "timeout");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "таймаут не сработал: {:?}",
            started.elapsed()
        );
    }

    /// Отмена гасит процесс так же, как таймаут.
    #[tokio::test]
    async fn cancellation_kills_the_process_too() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::mock_hang(dir.path(), "hang", 120).unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let err = run(&bin, &[], Duration::from_secs(60), &cancel).await.unwrap_err();
        assert_eq!(err.code, "cancelled");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Наблюдатель может снять процесс, НЕ ДОЖИДАЯСЬ его конца. Ровно на этом
    /// стоит Metal-гард.
    #[tokio::test]
    async fn the_watcher_can_kill_the_process_mid_run() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::write_mock_bin(
            dir.path(),
            "chatty",
            "echo 'всё хорошо' >&2\necho 'СТОП' >&2\nexec sleep 120\n",
        )
        .unwrap();
        let started = std::time::Instant::now();
        let err = run_watched(&bin, &[], Duration::from_secs(60), &CancellationToken::new(), |l| {
            if l.contains("СТОП") {
                LineVerdict::Kill("stopped", "наблюдатель попросил".into())
            } else {
                LineVerdict::Continue
            }
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, "stopped");
        assert!(err.stderr_tail.contains("всё хорошо"), "{}", err.stderr_tail);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "процесс не убит: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_named_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(
            &dir.path().join("нет-такого"),
            &[],
            Duration::from_secs(1),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "spawn_failed");
    }

    #[tokio::test]
    async fn stdout_capture_reads_the_answer() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let bin = mocks::mock_ffprobe(dir.path(), "ffprobe", 12.5).unwrap();
        let out = capture_stdout(&bin, &[], Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.trim(), "12.5");
    }

    /// **ТРИ ВАРИАНТА НА КАЖДЫЙ ИЗ ТРЁХ БИНАРЕЙ.**
    ///
    /// Один тест на всю матрицу, потому что проверяется одно свойство:
    /// внешний процесс любого рода либо доносит свой stderr, либо убивается
    /// по таймауту — и в обоих случаях вызывающий получает КОД и ТЕКСТ, а не
    /// молчание.
    ///
    /// claude здесь тоже: его пайплайн — работа Ф6, но инструмент обязан
    /// существовать и вести себя так же, иначе Ф6 начнётся с починки моков.
    #[tokio::test]
    async fn every_external_binary_has_all_three_behaviours() {
        let _g = guard();
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();

        // 1. ffmpeg
        let ok = mocks::mock_ffmpeg_success(dir.path(), "ffmpeg-ok").unwrap();
        let out = dir.path().join("out.wav");
        std::fs::write(dir.path().join("in.wav"), b"RIFFdata").unwrap();
        run(
            &ok,
            &[
                "-i".into(),
                dir.path().join("in.wav").display().to_string(),
                out.display().to_string(),
            ],
            Duration::from_secs(5),
            &cancel,
        )
        .await
        .unwrap();
        assert!(out.is_file(), "успешный ffmpeg обязан оставить файл");

        let bad = mocks::mock_ffmpeg_failure(dir.path(), "ffmpeg-bad", "Invalid data found").unwrap();
        let err = run(&bad, &[], Duration::from_secs(5), &cancel).await.unwrap_err();
        assert!(err.stderr_tail.contains("Invalid data found"), "{err}");

        let hang = mocks::mock_ffmpeg_hang(dir.path(), "ffmpeg-hang", 120).unwrap();
        let err = run(&hang, &[], Duration::from_millis(200), &cancel).await.unwrap_err();
        assert_eq!(err.code, "timeout");

        // 2. whisper — успех и падение проверяются целиком в `whisper.rs`
        //    (там же живёт Metal-гард); здесь важно, что хвост доносится тем
        //    же механизмом.
        let w = mocks::mock_whisper(
            dir.path(),
            "whisper-bad",
            &mocks::WhisperMock::failing("error: failed to load model"),
        )
        .unwrap();
        let err = run(&w, &[], Duration::from_secs(5), &cancel).await.unwrap_err();
        assert!(err.stderr_tail.contains("failed to load model"), "{err}");

        let w = mocks::mock_whisper(dir.path(), "whisper-hang", &mocks::WhisperMock::hanging(120))
            .unwrap();
        let err = run(&w, &[], Duration::from_millis(200), &cancel).await.unwrap_err();
        assert_eq!(err.code, "timeout");

        // 3. claude
        let c = mocks::mock_claude_success(dir.path(), "claude-ok", "итог встречи").unwrap();
        run(&c, &[], Duration::from_secs(5), &cancel).await.unwrap();

        let c = mocks::mock_claude_failure(dir.path(), "claude-bad", "Error: usage limit reached")
            .unwrap();
        let err = run(&c, &[], Duration::from_secs(5), &cancel).await.unwrap_err();
        assert!(err.stderr_tail.contains("usage limit"), "{err}");

        let c = mocks::mock_claude_hang(dir.path(), "claude-hang", 120).unwrap();
        let err = run(&c, &[], Duration::from_millis(200), &cancel).await.unwrap_err();
        assert_eq!(err.code, "timeout");
    }

    /// Код Metal превращается в 409 лестницы ошибок, а не в 500.
    #[test]
    fn the_metal_code_becomes_a_dependency_error() {
        let e = ProcError::new("metal_unavailable", "GPU не подтверждён", "хвост");
        assert_eq!(
            MeetError::from(e),
            MeetError::DepUnavailable("metal_unavailable")
        );
        let e = ProcError::new("exit_failed", "упал", "Invalid data");
        match MeetError::from(e) {
            MeetError::Internal(m) => assert!(m.contains("Invalid data"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
