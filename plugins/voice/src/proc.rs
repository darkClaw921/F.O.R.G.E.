//! Запуск внешних процессов (whisper, ffmpeg, ffprobe).
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # ХВОСТ STDERR ДОХОДИТ ДО ПОЛЬЗОВАТЕЛЯ
//!
//! Не в лог сервера, а в кадр `error` плагина. Разница практическая: ffmpeg на
//! битом файле пишет «Invalid data found when processing input» последней
//! строкой из сорока, и без этой строки человек видит только «не
//! обработалось» — а дальше ему нечего делать, кроме как писать в поддержку.
//!
//! Именно ХВОСТ, а не начало: у ffmpeg первые строки — это баннер версии и
//! перечисление кодеков, причина всегда в конце.
//!
//! # Зависший процесс УБИВАЕТСЯ
//!
//! `kill_on_drop(true)` плюс явный `kill()` по таймауту. Без этого зависший
//! ffmpeg держал бы слот и pipe stderr до конца жизни devforge.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Сколько последних строк stderr показывается пользователю.
///
/// Двадцать: меньше — теряется контекст ошибки, больше — пользователю приезжает
/// простыня из прогресса ffmpeg.
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

impl std::error::Error for ProcError {}

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
/// Кольцо, а не `String`: whisper с `-pp` печатает прогресс сотнями строк, и на
/// длинной записи полный вывод — это мегабайты, из которых нужны последние
/// двадцать строк.
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

/// Запускает процесс И ЧИТАЕТ ЕГО STDOUT В ПАМЯТЬ, наблюдая за таймаутом.
///
/// Отличие от [`capture_stdout`] — двоичный, а не текстовый результат: этим
/// читается PCM с конвейера ffmpeg при расчёте огибающей ([`crate::waveform`]),
/// где перегонять сотни килобайт через `String::from_utf8_lossy` значило бы
/// испортить каждый второй сэмпл.
pub async fn capture_stdout_bytes(
    bin: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<Vec<u8>, ProcError> {
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
        Ok(Ok(out)) if out.status.success() => Ok(out.stdout),
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
