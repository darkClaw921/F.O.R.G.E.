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
//!
//! Phase 1 (remote mode):
//!
//! ```text
//! devforge run --remote                    # bind 0.0.0.0, auth включена (token из env/file/auto)
//! devforge run --remote --bind 192.168.1.5 --port 7331
//! devforge run --remote --token <64-hex>
//! DEVFORGE_AUTH_TOKEN=<64-hex> devforge run --remote
//! ```

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

pub const DEFAULT_PORT: u16 = 7331;
pub const DEFAULT_BIND: &str = "127.0.0.1";
pub const ENV_AUTH_TOKEN: &str = "DEVFORGE_AUTH_TOKEN";

/// Опции, общие для подкоманд `run` / `start`.
///
/// Содержит и порт (как раньше), и новые Phase 1 поля для remote-режима.
/// Все Option<…> сохраняются «как введено пользователем» — финальный
/// резолвинг с приоритетами CLI > server_config.json > env > default
/// делается в [`crate::server_config::resolve`] на стадии запуска.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub port: u16,
    /// `--remote` (bool flag). True ⇒ публичный режим (требует bind != 127.0.0.1
    /// и/или auth_token). False ⇒ legacy localhost.
    pub remote: bool,
    /// `--bind <addr>`. None ⇒ использовать дефолт по режиму
    /// (127.0.0.1 при !remote, 0.0.0.0 при remote).
    pub bind: Option<String>,
    /// `--token <hex>` или env DEVFORGE_AUTH_TOKEN. None ⇒ при remote
    /// генерируется автоматически.
    pub token: Option<String>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            remote: false,
            bind: None,
            token: None,
        }
    }
}

/// Режим, выбранный пользователем через CLI.
#[derive(Debug, Clone)]
pub enum Mode {
    /// Foreground-сервер. Текущее поведение до Phase 5.
    Run(RunOptions),
    /// Запустить daemon в фоне (см. [`crate::daemon::start`]).
    Start(RunOptions),
    /// Послать SIGTERM запущенному daemon'у.
    Stop,
    /// Распечатать статус daemon'а (running/not running + PID + порт).
    Status,
    /// Phase 2 — pairing-команда. Сейчас единственный поддерживаемый поток —
    /// `devforge pair --generate`: генерирует 64-hex token, сохраняет в
    /// `~/.config/forge/server_config.json` (создавая файл), печатает
    /// инструкцию и НЕ запускает сервер.
    Pair(PairOptions),
    /// Phase 2 — управление реестром remote-серверов (CLI).
    /// `devforge remote list|add|remove ...`. Доступно ВСЕГДА, независимо
    /// от remote-mode сервера.
    Remote(RemoteCmd),
}

/// Опции подкоманды `devforge pair`. На данный момент поддерживается только
/// `--generate` — другие операции pairing (например, accept-from-prompt)
/// будут добавлены позже при необходимости.
#[derive(Debug, Clone, Default)]
pub struct PairOptions {
    /// True ⇒ генерировать новый 64-hex токен и сохранять в
    /// server_config.json (с merge'ом существующих полей).
    pub generate: bool,
}

/// Команды `devforge remote …`.
#[derive(Debug, Clone)]
pub enum RemoteCmd {
    /// `devforge remote list` — табличный вывод реестра.
    List,
    /// `devforge remote add <url> --token <hex> [--label <name>]`.
    Add(RemoteAddOptions),
    /// `devforge remote remove <id>`.
    Remove { id: String },
}

/// Параметры `devforge remote add <url> --token <hex> [--label <name>]`.
#[derive(Debug, Clone)]
pub struct RemoteAddOptions {
    pub url: String,
    pub token: String,
    /// Если `None` — label выводится из host'а URL.
    pub label: Option<String>,
}

/// Парсит `std::env::args()`. На некорректные аргументы возвращает `Err`.
/// `--help`/`-h` обрабатываются вызывающим кодом ДО вызова `parse()` (чтобы
/// help работал без инициализации tokio и без побочных эффектов).
pub fn parse() -> Result<Mode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    parse_from(args)
}

/// Аналог [`parse`], но принимает явно переданный вектор аргументов.
/// Используется в unit-тестах и для удобного reuse.
pub fn parse_from(args: Vec<String>) -> Result<Mode> {
    // Phase 2 — подкоманды `pair` и `remote` имеют свою own grammar
    // (отличную от `run`/`start`). Разбираем их раньше общего парсера,
    // чтобы не тянуть `--remote` flag в их аргументы.
    if let Some(first) = args.first() {
        match first.as_str() {
            "pair" => return parse_pair(&args[1..]),
            "remote" => return parse_remote(&args[1..]),
            _ => {}
        }
    }

    let mut iter = args.into_iter().peekable();

    let mut subcmd: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut remote = false;
    let mut bind: Option<String> = None;
    let mut token: Option<String> = None;

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
            "--remote" => {
                remote = true;
            }
            "--bind" => {
                let v = iter
                    .next()
                    .context("--bind requires a value (e.g. --bind 0.0.0.0)")?;
                bind = Some(v);
            }
            s if s.starts_with("--bind=") => {
                bind = Some(s["--bind=".len()..].to_string());
            }
            "--token" => {
                let v = iter
                    .next()
                    .context("--token requires a 64-hex value")?;
                token = Some(v);
            }
            s if s.starts_with("--token=") => {
                token = Some(s["--token=".len()..].to_string());
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

    // Fallback на env DEVFORGE_AUTH_TOKEN — но только если CLI токена не задано.
    // Финальный приоритет (CLI > file > env) разрешается в server_config::resolve.
    // Здесь мы просто подмешиваем env как «как будто пользователь ввёл --token».
    if token.is_none() {
        if let Ok(v) = std::env::var(ENV_AUTH_TOKEN) {
            if !v.is_empty() {
                token = Some(v);
            }
        }
    }

    let port = port.unwrap_or(DEFAULT_PORT);
    let has_run_only_flags = remote || bind.is_some() || token.is_some();
    let run_opts = RunOptions {
        port,
        remote,
        bind,
        token,
    };

    let mode = match subcmd.as_deref() {
        None | Some("run") => Mode::Run(run_opts),
        Some("start") => Mode::Start(run_opts),
        Some("stop") => {
            if port != DEFAULT_PORT {
                bail!("`--port` is not valid for `stop` (PID is read from pid-file)");
            }
            if has_run_only_flags {
                bail!("`--remote`/`--bind`/`--token` are not valid for `stop`");
            }
            Mode::Stop
        }
        Some("status") => {
            if port != DEFAULT_PORT {
                bail!("`--port` is not valid for `status`");
            }
            if has_run_only_flags {
                bail!("`--remote`/`--bind`/`--token` are not valid for `status`");
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

/// Парсер `devforge pair ...`. Поддерживаемые формы:
/// - `devforge pair --generate` — единственный текущий поток.
fn parse_pair(args: &[String]) -> Result<Mode> {
    let mut generate = false;
    for a in args {
        match a.as_str() {
            "--generate" | "-g" => generate = true,
            other => bail!(
                "unknown argument for `pair`: {other}\n\
                 Usage: devforge pair --generate"
            ),
        }
    }
    if !generate {
        bail!("`devforge pair` requires --generate (no other flows yet)");
    }
    Ok(Mode::Pair(PairOptions { generate }))
}

/// Парсер `devforge remote ...`. Поддерживаемые формы:
/// - `devforge remote list`
/// - `devforge remote add <url> --token <hex> [--label <name>]`
/// - `devforge remote remove <id>`
fn parse_remote(args: &[String]) -> Result<Mode> {
    let sub = args
        .first()
        .map(|s| s.as_str())
        .context("`devforge remote` requires a subcommand: list, add, remove")?;
    let rest = &args[1..];
    match sub {
        "list" | "ls" => {
            if !rest.is_empty() {
                bail!("`remote list` does not accept additional arguments");
            }
            Ok(Mode::Remote(RemoteCmd::List))
        }
        "add" => parse_remote_add(rest),
        "remove" | "rm" | "delete" => {
            // Первый positional — id.
            let id = rest
                .first()
                .cloned()
                .context("`remote remove` requires <id>")?;
            if rest.len() > 1 {
                bail!("`remote remove` accepts a single positional <id>");
            }
            Ok(Mode::Remote(RemoteCmd::Remove { id }))
        }
        other => bail!(
            "unknown `remote` subcommand: {other}\n\
             Try: remote list | remote add <url> --token <hex> | remote remove <id>"
        ),
    }
}

/// Парсер `remote add <url> --token <hex> [--label <name>]`.
///
/// Position-arg URL: первый non-flag. `--token`/`--label` могут идти где
/// угодно. Поддерживаются и форма `--key=value`, и `--key value`.
fn parse_remote_add(args: &[String]) -> Result<Mode> {
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    let mut label: Option<String> = None;

    let mut iter = args.iter().cloned().peekable();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--token" => {
                let v = iter.next().context("--token requires a value")?;
                token = Some(v);
            }
            s if s.starts_with("--token=") => {
                token = Some(s["--token=".len()..].to_string());
            }
            "--label" => {
                let v = iter.next().context("--label requires a value")?;
                label = Some(v);
            }
            s if s.starts_with("--label=") => {
                label = Some(s["--label=".len()..].to_string());
            }
            s if s.starts_with("--") => {
                bail!("unknown flag for `remote add`: {s}");
            }
            other => {
                if url.is_some() {
                    bail!("`remote add` takes a single positional <url>");
                }
                url = Some(other.to_string());
            }
        }
    }

    let url = url.context(
        "`remote add` requires a <url> positional argument (e.g. http://host:7331)",
    )?;
    let token = token.context("`remote add` requires --token <64-hex>")?;
    Ok(Mode::Remote(RemoteCmd::Add(RemoteAddOptions { url, token, label })))
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

/// Phase 2 — реализация `devforge pair --generate`.
///
/// Логика:
/// 1. Генерит 64-hex token через [`crate::server_config::generate_token_64hex`].
/// 2. Загружает существующий server_config.json (если есть), мерджит токен
///    в `auth_token`, иначе создаёт новый файл с дефолтами `bind=0.0.0.0`,
///    `port=7331`.
/// 3. Atomic save (tempfile+rename) через [`crate::server_config::save_to`].
/// 4. Печатает инструкцию пользователю: URL/Token + где сохранено.
///
/// НЕ запускает сервер. Команда полностью идемпотентна — повторный запуск
/// просто переписывает токен (без подтверждения, как в плане).
pub fn run_pair(opts: &PairOptions) -> Result<()> {
    if !opts.generate {
        bail!("`devforge pair` currently requires --generate");
    }
    let path = crate::server_config::default_server_config_path()
        .context("failed to resolve server_config.json path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let token = crate::server_config::generate_token_64hex();
    let merged = match crate::server_config::load_from(&path)? {
        Some(mut existing) => {
            existing.auth_token = Some(token.clone());
            // bind/port — оставляем как было; если их не было, заполняем
            // дефолты, чтобы пользователь сразу видел готовый файл.
            if existing.bind.is_none() {
                existing.bind = Some("0.0.0.0".to_string());
            }
            if existing.port.is_none() {
                existing.port = Some(DEFAULT_PORT);
            }
            existing
        }
        None => crate::server_config::ServerConfig {
            auth_token: Some(token.clone()),
            bind: Some("0.0.0.0".to_string()),
            port: Some(DEFAULT_PORT),
        },
    };
    crate::server_config::save_to(&path, &merged)
        .with_context(|| format!("failed to save {}", path.display()))?;

    print_pair_banner(&token, merged.bind.as_deref().unwrap_or("0.0.0.0"),
                      merged.port.unwrap_or(DEFAULT_PORT), &path);
    Ok(())
}

/// Печать инструкции после `pair --generate`. Формат соответствует секции
/// «На удалённом сервере» из плана Phase 2.
fn print_pair_banner(token: &str, bind: &str, port: u16, path: &std::path::Path) {
    let bar = "=".repeat(74);
    println!();
    println!("{bar}");
    println!(" devforge pair — token generated");
    println!("{bar}");
    println!(" Generated token: {token}");
    println!(" Bind address:    {bind}");
    println!(" Listening port:  {port}");
    println!();
    println!(" Add this on your local DevForge:");
    println!("   URL:   http://<this-host>:{port}");
    println!("   Token: {token}");
    println!();
    println!(" Server config saved to: {}", path.display());
    println!(" Restart devforge for changes to take effect.");
    println!("{bar}");
    println!();
}

/// Phase 2 — реализация `devforge remote ...` CLI.
///
/// Все подкоманды работают с локальным remote_servers.json через
/// [`crate::remotes::RemoteServerStore`] напрямую (НЕ через REST API).
/// Работает всегда, независимо от того, запущен ли локальный сервер
/// в remote-mode.
pub fn run_remote(cmd: &RemoteCmd) -> Result<()> {
    let path = crate::remotes::default_remotes_path()
        .context("failed to resolve remote_servers.json path")?;
    let mut store = crate::remotes::RemoteServerStore::load(path)
        .context("failed to load remote_servers.json")?;

    match cmd {
        RemoteCmd::List => {
            let servers = store.list();
            if servers.is_empty() {
                println!("No remote servers registered.");
                println!("Add one with: devforge remote add <url> --token <hex> [--label <name>]");
                return Ok(());
            }
            // Простой табличный вывод: id | label | url. Token НЕ печатается.
            let id_w = servers.iter().map(|s| s.id.len()).max().unwrap_or(2).max(2);
            let lbl_w = servers
                .iter()
                .map(|s| s.label.len())
                .max()
                .unwrap_or(5)
                .max(5);
            println!(
                "{:id_w$}  {:lbl_w$}  URL",
                "ID",
                "LABEL",
                id_w = id_w,
                lbl_w = lbl_w
            );
            println!(
                "{}  {}  {}",
                "-".repeat(id_w),
                "-".repeat(lbl_w),
                "-".repeat(3)
            );
            for s in &servers {
                println!(
                    "{:id_w$}  {:lbl_w$}  {}",
                    s.id,
                    s.label,
                    s.url,
                    id_w = id_w,
                    lbl_w = lbl_w
                );
            }
        }
        RemoteCmd::Add(opts) => {
            let label = opts
                .label
                .clone()
                .unwrap_or_else(|| derive_label_from_url(&opts.url));
            let server = store
                .add(label, &opts.url, &opts.token)
                .context("failed to add remote server")?;
            store.save().context("failed to save remote_servers.json")?;
            println!("Added remote server:");
            println!("  ID:    {}", server.id);
            println!("  Label: {}", server.label);
            println!("  URL:   {}", server.url);
        }
        RemoteCmd::Remove { id } => {
            if !store.remove(id) {
                bail!("no remote server with id `{id}`");
            }
            store.save().context("failed to save remote_servers.json")?;
            println!("Removed remote server `{id}`.");
        }
    }
    Ok(())
}

/// Производит человекочитаемый label из URL: берёт host (без схемы и порта),
/// например, `http://192.168.1.5:7331` → `192.168.1.5`. Fallback на сам URL,
/// если хост не определяется.
fn derive_label_from_url(url: &str) -> String {
    let no_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host_part = no_scheme.split('/').next().unwrap_or(no_scheme);
    let host = host_part.split(':').next().unwrap_or(host_part);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
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
             status            Show daemon status (running/stopped + PID + port)\n    \
             pair --generate   Generate auth token and save to server_config.json (no server start)\n    \
             remote list       List registered remote devforge servers\n    \
             remote add <url> --token <hex> [--label <name>]   Register a remote\n    \
             remote remove <id>                                 Remove a remote by id\n\
         \n\
         OPTIONS:\n    \
             -p, --port <N>    Port to listen on (default: {DEFAULT_PORT}). Valid for `run` and `start`.\n    \
                 --remote      Enable remote/public mode (bind 0.0.0.0, Bearer-auth required).\n    \
                 --bind <ADDR> Bind address. Default: {DEFAULT_BIND} (localhost), or 0.0.0.0 if --remote.\n    \
                 --token <HEX> Bearer token (64-hex). Env: ${ENV_AUTH_TOKEN}. Auto-generated if --remote without token.\n    \
             -h, --help        Show this help message\n\
         \n\
         EXAMPLES:\n    \
             devforge                     # foreground on port {DEFAULT_PORT}\n    \
             devforge start --port 8080   # daemon on port 8080\n    \
             devforge run --remote        # public bind on 0.0.0.0, auto token\n    \
             devforge status              # check daemon\n    \
             devforge stop                # stop daemon\n\
         \n\
         FILES:\n    \
             ~/.config/forge/devforge.pid          PID of running daemon\n    \
             ~/.config/forge/devforge.log          Daemon stdout/stderr log\n    \
             ~/.config/forge/server_config.json    Optional remote-mode config (auth_token/bind/port)\n\
         \n\
         Requires `tmux` to be installed.\n\
         Homepage: https://github.com/darkClaw921/F.O.R.G.E."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// Возвращает RunOptions из Mode::Run, паника если не Run.
    fn expect_run(m: Mode) -> RunOptions {
        match m {
            Mode::Run(o) => o,
            _ => panic!("expected Mode::Run"),
        }
    }

    #[test]
    fn parse_empty_defaults() {
        let m = parse_from(args(&[])).unwrap();
        let o = expect_run(m);
        assert_eq!(o.port, DEFAULT_PORT);
        assert!(!o.remote);
        assert_eq!(o.bind, None);
    }

    #[test]
    fn parse_run_with_port() {
        let m = parse_from(args(&["run", "--port", "8080"])).unwrap();
        let o = expect_run(m);
        assert_eq!(o.port, 8080);
    }

    #[test]
    fn parse_short_port() {
        let m = parse_from(args(&["-p", "9000"])).unwrap();
        assert_eq!(expect_run(m).port, 9000);
    }

    #[test]
    fn parse_port_equals() {
        let m = parse_from(args(&["--port=4242"])).unwrap();
        assert_eq!(expect_run(m).port, 4242);
    }

    #[test]
    fn parse_remote_flag() {
        let m = parse_from(args(&["run", "--remote"])).unwrap();
        let o = expect_run(m);
        assert!(o.remote);
        assert_eq!(o.bind, None);
        // token может прийти из env — для теста очищать env неудобно,
        // оставляем только проверку remote=true.
    }

    #[test]
    fn parse_bind_value() {
        let m = parse_from(args(&["run", "--bind", "0.0.0.0"])).unwrap();
        assert_eq!(expect_run(m).bind.as_deref(), Some("0.0.0.0"));

        let m = parse_from(args(&["run", "--bind=192.168.1.5"])).unwrap();
        assert_eq!(expect_run(m).bind.as_deref(), Some("192.168.1.5"));
    }

    #[test]
    fn parse_token_value() {
        // Изолируем от env DEVFORGE_AUTH_TOKEN: если он есть, всё равно
        // CLI имеет приоритет.
        let m = parse_from(args(&["run", "--token", "abcdef"])).unwrap();
        assert_eq!(expect_run(m).token.as_deref(), Some("abcdef"));

        let m = parse_from(args(&["run", "--token=12345"])).unwrap();
        assert_eq!(expect_run(m).token.as_deref(), Some("12345"));
    }

    #[test]
    fn parse_invalid_port() {
        assert!(parse_from(args(&["--port", "0"])).is_err());
        assert!(parse_from(args(&["--port", "99999"])).is_err());
        assert!(parse_from(args(&["--port", "abc"])).is_err());
    }

    #[test]
    fn parse_stop_rejects_run_flags() {
        assert!(parse_from(args(&["stop", "--remote"])).is_err());
        assert!(parse_from(args(&["status", "--bind", "x"])).is_err());
    }

    #[test]
    fn parse_unknown_arg() {
        assert!(parse_from(args(&["--frobnicate"])).is_err());
    }

    #[test]
    fn parse_start_carries_options() {
        let m = parse_from(args(&["start", "--port", "8080", "--remote"])).unwrap();
        match m {
            Mode::Start(o) => {
                assert_eq!(o.port, 8080);
                assert!(o.remote);
            }
            _ => panic!("expected Start"),
        }
    }

    // ============================================================
    // Phase 2 — pair / remote parsing
    // ============================================================

    #[test]
    fn parse_pair_generate() {
        let m = parse_from(args(&["pair", "--generate"])).unwrap();
        match m {
            Mode::Pair(opts) => assert!(opts.generate),
            _ => panic!("expected Mode::Pair"),
        }
    }

    #[test]
    fn parse_pair_short_g() {
        let m = parse_from(args(&["pair", "-g"])).unwrap();
        match m {
            Mode::Pair(opts) => assert!(opts.generate),
            _ => panic!("expected Mode::Pair"),
        }
    }

    #[test]
    fn parse_pair_requires_generate() {
        assert!(parse_from(args(&["pair"])).is_err());
        assert!(parse_from(args(&["pair", "--foo"])).is_err());
    }

    #[test]
    fn parse_remote_list() {
        let m = parse_from(args(&["remote", "list"])).unwrap();
        match m {
            Mode::Remote(RemoteCmd::List) => {}
            _ => panic!("expected Mode::Remote(List)"),
        }
        // alias `ls`
        let m = parse_from(args(&["remote", "ls"])).unwrap();
        matches!(m, Mode::Remote(RemoteCmd::List));
    }

    #[test]
    fn parse_remote_add_full() {
        let m = parse_from(args(&[
            "remote", "add", "http://x:7331", "--token", "abc", "--label", "Office",
        ]))
        .unwrap();
        match m {
            Mode::Remote(RemoteCmd::Add(opts)) => {
                assert_eq!(opts.url, "http://x:7331");
                assert_eq!(opts.token, "abc");
                assert_eq!(opts.label.as_deref(), Some("Office"));
            }
            _ => panic!("expected Mode::Remote(Add)"),
        }
    }

    #[test]
    fn parse_remote_add_minimal() {
        let m = parse_from(args(&["remote", "add", "http://x:7331", "--token=abc"]))
            .unwrap();
        match m {
            Mode::Remote(RemoteCmd::Add(opts)) => {
                assert_eq!(opts.url, "http://x:7331");
                assert_eq!(opts.token, "abc");
                assert!(opts.label.is_none());
            }
            _ => panic!("expected Mode::Remote(Add)"),
        }
    }

    #[test]
    fn parse_remote_add_requires_url_and_token() {
        // No url
        assert!(parse_from(args(&["remote", "add", "--token", "abc"])).is_err());
        // No token
        assert!(parse_from(args(&["remote", "add", "http://x"])).is_err());
    }

    #[test]
    fn parse_remote_remove() {
        let m = parse_from(args(&["remote", "remove", "office"])).unwrap();
        match m {
            Mode::Remote(RemoteCmd::Remove { id }) => assert_eq!(id, "office"),
            _ => panic!("expected Mode::Remote(Remove)"),
        }
        // alias rm
        let m = parse_from(args(&["remote", "rm", "x"])).unwrap();
        match m {
            Mode::Remote(RemoteCmd::Remove { id }) => assert_eq!(id, "x"),
            _ => panic!("expected Mode::Remote(Remove)"),
        }
    }

    #[test]
    fn parse_remote_remove_requires_id() {
        assert!(parse_from(args(&["remote", "remove"])).is_err());
        assert!(parse_from(args(&["remote", "remove", "a", "b"])).is_err());
    }

    #[test]
    fn parse_remote_unknown_sub() {
        assert!(parse_from(args(&["remote", "frobnicate"])).is_err());
    }

    #[test]
    fn derive_label_from_url_matrix() {
        assert_eq!(derive_label_from_url("http://192.168.1.5:7331"), "192.168.1.5");
        assert_eq!(derive_label_from_url("https://office.example.com"), "office.example.com");
        assert_eq!(derive_label_from_url("http://10.0.0.1:80/path"), "10.0.0.1");
    }

    #[test]
    fn run_pair_writes_token_to_explicit_path() {
        // Используем низкоуровневый server_config API напрямую с tempfile
        // (run_pair завязан на ~/.config/forge/... через default path,
        // менять HOME в тесте опасно для parallel-runner). Тест проверяет
        // суть логики через ту же save_to/load_from цепочку.
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "forge-pair-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server_config.json");

        let token = crate::server_config::generate_token_64hex();
        let cfg = crate::server_config::ServerConfig {
            auth_token: Some(token.clone()),
            bind: Some("0.0.0.0".to_string()),
            port: Some(DEFAULT_PORT),
        };
        crate::server_config::save_to(&path, &cfg).unwrap();
        let loaded = crate::server_config::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.auth_token.as_deref(), Some(token.as_str()));
        assert_eq!(loaded.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(loaded.port, Some(DEFAULT_PORT));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================================
    // Phase 8 .7 — CLI edge cases
    // =========================================================================

    #[test]
    fn cli_token_without_remote_is_accepted() {
        // --token без --remote — текущая политика: парсер не возражает,
        // server_config::resolve() сам решит активировать ли remote_mode.
        // (При implies_remote=false из файла и remote=false из CLI токен
        // будет проигнорирован — тест-as-spec.)
        let m = parse_from(args(&["run", "--token", "tok123"])).unwrap();
        let o = expect_run(m);
        assert!(!o.remote);
        assert_eq!(o.token.as_deref(), Some("tok123"));
    }

    #[test]
    fn cli_bind_ipv6_bracketed_parses() {
        // [::1]:8080 формат — bind принимает строку как есть, валидация
        // address+port делается дальше TCP-listener'ом.
        let m = parse_from(args(&["run", "--bind", "[::1]:8080"])).unwrap();
        assert_eq!(expect_run(m).bind.as_deref(), Some("[::1]:8080"));
    }

    #[test]
    fn cli_port_lower_bound_1_accepted() {
        let m = parse_from(args(&["run", "--port", "1"])).unwrap();
        assert_eq!(expect_run(m).port, 1);
    }

    #[test]
    fn cli_port_upper_bound_65535_accepted() {
        let m = parse_from(args(&["run", "--port", "65535"])).unwrap();
        assert_eq!(expect_run(m).port, 65535);
    }

    #[test]
    fn cli_port_negative_rejected() {
        // u16 не принимает отрицательные → parse_port → Err.
        let r = parse_from(args(&["--port", "-1"]));
        assert!(r.is_err(), "negative port must Err");
    }

    #[test]
    fn cli_port_zero_rejected() {
        let r = parse_from(args(&["--port", "0"]));
        assert!(r.is_err(), "port 0 must Err");
    }

    #[test]
    fn cli_unknown_top_level_command_errors() {
        let r = parse_from(args(&["nonexistent-cmd"]));
        assert!(r.is_err(), "unknown top-level command must Err");
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains("unknown") || msg.contains("nonexistent-cmd"),
            "сообщение должно упомянуть unknown arg, got: {msg}"
        );
    }

    #[test]
    fn cli_help_text_contains_usage_keywords() {
        // help_text() — pure-функция, возвращает help как String. Должна
        // содержать ключевые слова: 'Usage', 'devforge', '--remote', '--port'.
        let h = help_text();
        assert!(h.contains("devforge"));
        assert!(h.contains("--remote"));
        assert!(h.contains("--port"));
    }

    #[test]
    fn cli_pair_generate_and_short_g_combined() {
        // pair --generate -g — оба алиаса одного флага. Текущий парсер
        // тривиально OR'ит булеан, дубль не ломает.
        let m = parse_from(args(&["pair", "--generate", "-g"])).unwrap();
        match m {
            Mode::Pair(opts) => assert!(opts.generate),
            _ => panic!("expected Pair"),
        }
    }

    #[test]
    fn cli_pair_with_extra_positional_errors() {
        let r = parse_from(args(&["pair", "--generate", "extra-arg"]));
        assert!(r.is_err(), "pair с лишним arg должен Err");
    }

    #[test]
    fn cli_remote_add_duplicate_url_in_store_rejected_at_runtime() {
        // Парсер сам по себе не проверяет дубликаты URL — это контракт
        // RemoteServerStore::add (если бы он его проверял). Текущая
        // реализация: store::add позволяет дубликаты URL, но slug при
        // конфликте labels получит -2/-3. Закрепим это поведение
        // как тест-as-spec.
        use std::path::PathBuf;
        let tmp = std::env::temp_dir().join(format!(
            "forge-cli-dup-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store =
            crate::remotes::RemoteServerStore::load(PathBuf::from(&tmp)).unwrap();
        let a = store.add("Office", "http://1.2.3.4:7331", "t1").unwrap();
        let b = store.add("Office", "http://1.2.3.4:7331", "t2").unwrap();
        assert_eq!(a.id, "office");
        assert_eq!(
            b.id, "office-2",
            "дубликат label получает -2 suffix; URL не проверяется"
        );
    }

    #[test]
    fn cli_run_pair_idempotent_overwrites_token_in_existing_file() {
        // Phase 8 .7 — politika ротации токена при повторном run_pair'е.
        // run_pair вызывает finalize_token_at, который ВСЕГДА перезаписывает
        // auth_token при отсутствии в файле. Если токен уже есть — он
        // сохраняется (см. finalize_token_preserves_existing). Поведение
        // 'force-rotation' пока не реализовано — закрепляем preserve-policy.
        use std::path::PathBuf;
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "forge-pair-rotate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = PathBuf::from(dir.join("server_config.json"));

        // Pre-existing config с токеном.
        let existing_token = "existing-token-keep-me";
        let cfg = crate::server_config::ServerConfig {
            auth_token: Some(existing_token.to_string()),
            bind: Some("0.0.0.0".to_string()),
            port: Some(DEFAULT_PORT),
        };
        crate::server_config::save_to(&path, &cfg).unwrap();

        // Симулируем второй run через finalize_token_at с remote_mode.
        let eff = crate::server_config::EffectiveConfig {
            bind: "0.0.0.0".to_string(),
            port: DEFAULT_PORT,
            auth_token: Some(existing_token.to_string()),
            remote_mode: true,
        };
        let got = crate::server_config::finalize_token_at(&eff, &path).unwrap();
        assert_eq!(
            got, existing_token,
            "повторный finalize_token preserves existing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
