//! Daemon-режим `devforge start` / `stop` / `status`.
//!
//! Подход: процесс-родитель спавнит сам себя как `devforge run --port N` с
//! stdout/stderr в лог-файл и вызывает `setsid(2)` через `CommandExt::pre_exec`,
//! чтобы дочерний процесс отвязался от controlling terminal и пережил
//! закрытие shell. PID пишется в `~/.config/forge/devforge.pid`.
//!
//! Это сознательно простой подход: без double-fork, без tty-redirect через
//! /dev/null (stdin=null хватает). Для пользовательского use case
//! «запустил-выключил» — достаточно. Если когда-то потребуется service-style
//! управление — это уже launchd / systemd, а не наша забота.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::cli;

/// Запустить daemon в фоне с заданными опциями. Если pid-файл существует и
/// процесс жив — отказ. Если pid-файл есть, но процесс мёртв — pid-файл
/// удаляется и старт продолжается (нормальный recovery после краша).
///
/// Все опции (`--port`, `--remote`, `--bind`, `--token`) прокидываются в
/// child-процесс `devforge run …`, чтобы он точно унаследовал режим, выбранный
/// пользователем. Без этого `devforge start --remote` молча превращался бы в
/// localhost-режим, что нарушает single-source-of-truth для CLI.
pub fn start(opts: &cli::RunOptions) -> Result<()> {
    let port = opts.port;
    let pid_path = cli::pid_path()?;
    let log_path = cli::log_path()?;
    let state_dir = cli::state_dir()?;

    fs::create_dir_all(&state_dir).with_context(|| {
        format!("failed to create state dir {}", state_dir.display())
    })?;

    if let Some(existing) = read_pid(&pid_path) {
        if is_alive(existing) {
            bail!(
                "devforge already running (PID {existing}). Stop it first:\n    devforge stop"
            );
        } else {
            // Stale pid-файл — переcoздаём.
            let _ = fs::remove_file(&pid_path);
        }
    }

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .context("failed to clone log file handle for stderr")?;

    let exe = std::env::current_exe().context("failed to resolve current_exe")?;
    let mut cmd = Command::new(&exe);
    // Базовая команда — всегда `run --port N`.
    cmd.arg("run").arg("--port").arg(port.to_string());
    // Опционально пробрасываем remote-флаги. Это критично: иначе daemon
    // запустился бы в legacy-режиме даже после `devforge start --remote`.
    if opts.remote {
        cmd.arg("--remote");
    }
    if let Some(bind) = &opts.bind {
        cmd.arg("--bind").arg(bind);
    }
    if let Some(token) = &opts.token {
        cmd.arg("--token").arg(token);
    }
    cmd.stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() — async-signal-safe и не имеет UB-условий, кроме
        // EPERM (когда процесс уже лидер группы). Между fork() и exec() мы
        // единственный поток, поэтому это безопасно.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .context("failed to spawn devforge daemon process")?;
    let pid = child.id();

    // Записываем pid-файл атомарно (write + rename). Если упадём между этими
    // строками — следующий start увидит «stale» через is_alive() и поправит.
    let tmp = pid_path.with_extension("pid.tmp");
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        writeln!(f, "{pid}")?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, &pid_path).with_context(|| {
        format!("failed to write pid-file {}", pid_path.display())
    })?;

    // Короткая пауза — чтобы daemon успел либо забиндиться, либо упасть с
    // сообщением в логе. Если за 600ms процесс умер — выводим хвост лога и
    // удаляем pid-файл.
    std::thread::sleep(Duration::from_millis(600));
    if !is_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        let tail = read_tail(&log_path, 40).unwrap_or_default();
        bail!(
            "daemon exited immediately. Tail of {}:\n{tail}",
            log_path.display()
        );
    }

    println!("devforge started.");
    println!("  PID:  {pid}");
    println!("  Port: {port}  →  http://127.0.0.1:{port}");
    println!("  Log:  {}", log_path.display());
    println!("  Stop: devforge stop");
    Ok(())
}

/// SIGTERM по pid-файлу. До 5 сек ждёт корректного завершения, потом SIGKILL.
pub fn stop() -> Result<()> {
    let pid_path = cli::pid_path()?;
    let pid = match read_pid(&pid_path) {
        Some(p) => p,
        None => {
            println!("devforge is not running (no pid-file at {}).", pid_path.display());
            return Ok(());
        }
    };

    if !is_alive(pid) {
        println!("stale pid-file (PID {pid} not running); cleaning up.");
        let _ = fs::remove_file(&pid_path);
        return Ok(());
    }

    // SIGTERM
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) == -1 {
            let err = std::io::Error::last_os_error();
            bail!("kill({pid}, SIGTERM) failed: {err}");
        }
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !is_alive(pid) {
            let _ = fs::remove_file(&pid_path);
            println!("devforge stopped (PID {pid}).");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // SIGKILL fallback
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGKILL) == -1 {
            let err = std::io::Error::last_os_error();
            bail!("kill({pid}, SIGKILL) failed: {err}");
        }
    }
    let _ = fs::remove_file(&pid_path);
    println!("devforge force-killed (PID {pid}, no response to SIGTERM in 5s).");
    Ok(())
}

/// Распечатать статус daemon'а. Никогда не возвращает ошибку — для использования
/// в shell-цепочках типа `devforge status && echo ok`.
pub fn status() -> Result<()> {
    let pid_path = cli::pid_path()?;
    match read_pid(&pid_path) {
        Some(pid) if is_alive(pid) => {
            println!("devforge: running (PID {pid})");
            println!("  Pid-file: {}", pid_path.display());
            println!("  Log:      {}", cli::log_path()?.display());
        }
        Some(pid) => {
            println!("devforge: not running (stale pid-file: {pid}). Run `devforge stop` to clean up.");
        }
        None => {
            println!("devforge: not running.");
        }
    }
    Ok(())
}

fn read_pid(path: &Path) -> Option<u32> {
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse().ok()
}

/// `kill(pid, 0)` — POSIX-проверка «процесс существует и доступен для сигнала».
#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    // Daemon-режим на не-Unix не поддерживаем (см. cfg target в Cargo.toml).
    false
}

fn read_tail(path: &Path, lines: usize) -> Result<String> {
    let s = fs::read_to_string(path)?;
    let collected: Vec<&str> = s.lines().rev().take(lines).collect();
    Ok(collected.into_iter().rev().collect::<Vec<_>>().join("\n"))
}
