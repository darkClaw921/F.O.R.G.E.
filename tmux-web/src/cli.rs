//! Парсинг CLI-аргументов для `devforge`.
//!
//! Без `clap` — чтобы не тащить зависимость ради 4 подкоманд и одного флага.
//! Стиль совместим с существующей ручной обработкой `--help` (см. main.rs).
//!
//! Поддерживаемые формы:
//!
//! ```text
//! devforge                       # foreground, дефолтный порт
//! devforge run                   # то же, явная подкоманда
//! devforge run --port 8080
//! devforge -p 8080
//! devforge start                 # daemon в фоне
//! devforge start --port 8080
//! devforge stop
//! devforge status
//! devforge --help | -h
//! ```

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

pub const DEFAULT_PORT: u16 = 7331;

/// Режим, выбранный пользователем через CLI.
#[derive(Debug, Clone)]
pub enum Mode {
    /// Foreground-сервер. Текущее поведение до Phase 5.
    Run { port: u16 },
    /// Запустить daemon в фоне (см. [`crate::daemon::start`]).
    Start { port: u16 },
    /// Послать SIGTERM запущенному daemon'у.
    Stop,
    /// Распечатать статус daemon'а (running/not running + PID + порт).
    Status,
}

/// Парсит `std::env::args()`. На некорректные аргументы возвращает `Err`.
/// `--help`/`-h` обрабатываются вызывающим кодом ДО вызова `parse()` (чтобы
/// help работал без инициализации tokio и без побочных эффектов).
pub fn parse() -> Result<Mode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = args.into_iter().peekable();

    let mut subcmd: Option<String> = None;
    let mut port: Option<u16> = None;

    while let Some(a) = iter.next() {
        match a.as_str() {
            "-p" | "--port" => {
                let v = iter
                    .next()
                    .context("--port requires a value (e.g. --port 8080)")?;
                port = Some(parse_port(&v)?);
            }
            s if s.starts_with("--port=") => {
                port = Some(parse_port(&s["--port=".len()..])?);
            }
            "run" | "start" | "stop" | "status" => {
                if subcmd.is_some() {
                    bail!("only one subcommand is allowed");
                }
                subcmd = Some(a);
            }
            // `--help`/`-h` ловится до `parse()` в main; если попало сюда —
            // значит идёт как часть длинной формы, что мы не поддерживаем.
            other => bail!(
                "unknown argument: {other}\n\nRun `devforge --help` for usage."
            ),
        }
    }

    let port = port.unwrap_or(DEFAULT_PORT);
    let mode = match subcmd.as_deref() {
        None | Some("run") => Mode::Run { port },
        Some("start") => Mode::Start { port },
        Some("stop") => {
            if port != DEFAULT_PORT {
                bail!("`--port` is not valid for `stop` (PID is read from pid-file)");
            }
            Mode::Stop
        }
        Some("status") => {
            if port != DEFAULT_PORT {
                bail!("`--port` is not valid for `status`");
            }
            Mode::Status
        }
        _ => unreachable!(),
    };
    Ok(mode)
}

fn parse_port(s: &str) -> Result<u16> {
    let n: u16 = s
        .parse()
        .with_context(|| format!("invalid port number: {s:?} (expected 1..=65535)"))?;
    if n == 0 {
        bail!("port 0 is not allowed");
    }
    Ok(n)
}

/// Каталог, в котором живут pid- и log-файлы daemon'а. Совпадает с папкой
/// `projects.json` (`~/.config/forge/` на Linux/macOS), что упрощает clean
/// uninstall и не плодит лишних мест.
pub fn state_dir() -> Result<PathBuf> {
    let registry = crate::projects::default_registry_path()?;
    let parent = registry
        .parent()
        .context("registry path has no parent directory")?
        .to_path_buf();
    Ok(parent)
}

/// `~/.config/forge/devforge.pid`
pub fn pid_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("devforge.pid"))
}

/// `~/.config/forge/devforge.log`
pub fn log_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("devforge.log"))
}

/// Текст для `--help`. Вынесен в функцию, чтобы переиспользовать в тестах
/// и в Homebrew formula `test do` блоке.
pub fn help_text() -> String {
    format!(
        "devforge — Flow Orchestration and Real-time Governance Engine\n\
         \n\
         USAGE:\n    \
             devforge [COMMAND] [OPTIONS]\n\
         \n\
         COMMANDS:\n    \
             run               Run server in foreground (default if no command given)\n    \
             start             Start server in background (daemon)\n    \
             stop              Stop the background server\n    \
             status            Show daemon status (running/stopped + PID + port)\n\
         \n\
         OPTIONS:\n    \
             -p, --port <N>    Port to listen on (default: {DEFAULT_PORT}). Valid for `run` and `start`.\n    \
             -h, --help        Show this help message\n\
         \n\
         EXAMPLES:\n    \
             devforge                     # foreground on port {DEFAULT_PORT}\n    \
             devforge start --port 8080   # daemon on port 8080\n    \
             devforge status              # check daemon\n    \
             devforge stop                # stop daemon\n\
         \n\
         FILES:\n    \
             ~/.config/forge/devforge.pid    PID of running daemon\n    \
             ~/.config/forge/devforge.log    Daemon stdout/stderr log\n\
         \n\
         Requires `tmux` to be installed.\n\
         Homepage: https://github.com/darkClaw921/F.O.R.G.E."
    )
}
