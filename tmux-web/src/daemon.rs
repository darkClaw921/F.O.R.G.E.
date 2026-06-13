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
        if is_recorded_process_alive(&existing) {
            let pid = existing.pid;
            bail!(
                "devforge already running (PID {pid}). Stop it first:\n    devforge stop"
            );
        } else {
            // Stale pid-файл (процесс мёртв или PID переиспользован чужим
            // процессом) — переcoздаём.
            let _ = fs::remove_file(&pid_path);
        }
    }

    // Лог может содержать чувствительные данные (URL с auth-токеном в баннере
    // при не-TTY-запуске и пр.) — создаём с правами 0600 (только владелец).
    let mut log_opts = fs::OpenOptions::new();
    log_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_opts.mode(0o600);
    }
    let log = log_opts
        .open(&log_path)
        .with_context(|| format!("failed to open log {}", log_path.display()))?;
    // mode() влияет только при создании; если файл уже существовал — выставим
    // 0600 явно, чтобы старые логи (созданные до этого фикса) тоже подтянулись.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600));
    }
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
    // Вместе с PID сохраняем start_time процесса — защита от PID-recycling:
    // `stop` сверит его перед kill и не убьёт чужой процесс, занявший наш PID.
    let start_time = process_start_time(pid);
    let tmp = pid_path.with_extension("pid.tmp");
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        match start_time {
            Some(st) => writeln!(f, "{pid} {st}")?,
            None => writeln!(f, "{pid}")?,
        }
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
    let rec = match read_pid(&pid_path) {
        Some(r) => r,
        None => {
            println!("devforge is not running (no pid-file at {}).", pid_path.display());
            return Ok(());
        }
    };
    let pid = rec.pid;

    // Сверяем, что PID всё ещё принадлежит НАШЕМУ процессу (по start_time).
    // Если процесс мёртв или PID переиспользован чужим процессом — не шлём
    // сигнал, просто чистим pid-файл.
    if !is_recorded_process_alive(&rec) {
        if is_alive(pid) {
            println!(
                "pid-file PID {pid} now belongs to a different process (recycled); \
                 not killing it. Cleaning up stale pid-file."
            );
        } else {
            println!("stale pid-file (PID {pid} not running); cleaning up.");
        }
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
        Some(rec) if is_recorded_process_alive(&rec) => {
            let pid = rec.pid;
            println!("devforge: running (PID {pid})");
            println!("  Pid-file: {}", pid_path.display());
            println!("  Log:      {}", cli::log_path()?.display());
        }
        Some(rec) => {
            let pid = rec.pid;
            println!("devforge: not running (stale pid-file: {pid}). Run `devforge stop` to clean up.");
        }
        None => {
            println!("devforge: not running.");
        }
    }
    Ok(())
}

/// Содержимое pid-файла. Формат: первая строка — `<pid>` (старый формат) или
/// `<pid> <start_time>` (новый). `start_time` — секунды старта процесса из ОС;
/// нужен для защиты от PID-recycling: ОС может переиспользовать освободившийся
/// PID под совершенно другой процесс, и `kill` тогда убьёт чужой. Сверяя
/// сохранённый start_time с фактическим, мы убеждаемся, что это всё ещё ТОТ
/// процесс, а не однофамилец-по-PID.
struct PidRecord {
    pid: u32,
    /// `None` — старый формат файла без start_time (graceful fallback).
    start_time: Option<u64>,
}

fn read_pid(path: &Path) -> Option<PidRecord> {
    let s = fs::read_to_string(path).ok()?;
    let line = s.lines().next()?.trim();
    let mut parts = line.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let start_time = parts.next().and_then(|s| s.parse::<u64>().ok());
    Some(PidRecord { pid, start_time })
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

/// Возвращает время старта процесса (секунды). Используется для защиты от
/// PID-recycling. `None`, если процесс не существует или информацию получить
/// не удалось (тогда вызывающий деградирует к проверке только по PID).
#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> Option<u64> {
    // proc_pidinfo(PROC_PIDTBSDINFO) → proc_bsdinfo.pbi_start_tvsec.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n == size {
        Some(info.pbi_start_tvsec)
    } else {
        None
    }
}

/// Linux: 22-е поле `/proc/<pid>/stat` (starttime, в clock ticks с момента
/// загрузки). Точное значение неважно — важно лишь, что оно стабильно для
/// данного процесса и различается при PID-recycling.
#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (поле 2) в скобках и может содержать пробелы → отрезаем по ')'.
    let after = stat.rsplit_once(')')?.1;
    // После ')' идёт " <state> ..."; starttime — 22-е поле, т.е. 20-е после comm.
    after.split_whitespace().nth(19)?.parse::<u64>().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_time(_pid: u32) -> Option<u64> {
    None
}

/// Жив ли именно ТОТ процесс, что записан в pid-файле (с учётом start_time).
/// При отсутствии сохранённого start_time (старый формат) — деградирует к
/// проверке только по PID (как было раньше).
fn is_recorded_process_alive(rec: &PidRecord) -> bool {
    if !is_alive(rec.pid) {
        return false;
    }
    match rec.start_time {
        Some(saved) => match process_start_time(rec.pid) {
            // start_time совпал — это наш процесс.
            Some(actual) => actual == saved,
            // Не смогли прочитать (гонка/нет прав) — не рискуем, считаем чужим
            // нельзя; но и убивать вслепую не будем — возвращаем false, чтобы
            // stop() не слал сигнал потенциально чужому PID.
            None => false,
        },
        // Старый формат — поведение как прежде.
        None => true,
    }
}

/// Читает последние `lines` строк лог-файла, НЕ загружая весь файл в память.
///
/// Лог daemon растёт без ротации и может стать многомегабайтным; прежняя
/// реализация делала `read_to_string` целиком ради 40 хвостовых строк. Теперь
/// читаем фиксированный хвостовой буфер через `seek(SeekFrom::End)`: размер
/// буфера растёт кратно, пока в нём не наберётся `lines+1` переводов строки или
/// не будет прочитан весь файл. На типичных строках лога (<256B) даже 40 строк
/// укладываются в первый же 16 KiB-чтение.
fn read_tail(path: &Path, lines: usize) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let file_len = f.metadata()?.len();
    if file_len == 0 {
        return Ok(String::new());
    }

    // Хотим lines строк → нужно lines+1 '\n' (или начало файла). Растим буфер.
    let mut chunk: u64 = 16 * 1024;
    loop {
        let read_len = chunk.min(file_len);
        let start = file_len - read_len;
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; read_len as usize];
        f.read_exact(&mut buf)?;

        let newline_count = buf.iter().filter(|&&b| b == b'\n').count();
        // Достаточно строк ИЛИ дочитали до начала файла.
        if newline_count > lines || read_len == file_len {
            let s = String::from_utf8_lossy(&buf);
            let collected: Vec<&str> = s.lines().rev().take(lines).collect();
            return Ok(collected.into_iter().rev().collect::<Vec<_>>().join("\n"));
        }
        chunk = chunk.saturating_mul(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pid_parses_old_format() {
        let dir = std::env::temp_dir().join(format!("forge-pid-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("devforge.pid");
        std::fs::write(&p, "12345\n").unwrap();
        let rec = read_pid(&p).expect("parse");
        assert_eq!(rec.pid, 12345);
        assert_eq!(rec.start_time, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pid_parses_new_format_with_start_time() {
        let dir = std::env::temp_dir().join(format!("forge-pid-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("devforge.pid");
        std::fs::write(&p, "999 1700000000\n").unwrap();
        let rec = read_pid(&p).expect("parse");
        assert_eq!(rec.pid, 999);
        assert_eq!(rec.start_time, Some(1700000000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_start_time_of_self_is_some_and_matches_record() {
        let pid = std::process::id();
        let st = process_start_time(pid).expect("own start_time");
        let rec = PidRecord { pid, start_time: Some(st) };
        // Наш собственный процесс жив и start_time совпадает.
        assert!(is_recorded_process_alive(&rec));
        // Несовпадающий start_time → считаем чужим.
        let rec_bad = PidRecord { pid, start_time: Some(st.wrapping_add(1)) };
        assert!(!is_recorded_process_alive(&rec_bad));
    }
}
