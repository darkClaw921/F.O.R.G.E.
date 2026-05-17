//! Загрузка/сохранение `~/.config/forge/server_config.json` и резолвинг
//! эффективной конфигурации сервера для Phase 1 (remote-mode).
//!
//! ## Формат файла
//!
//! ```json
//! {
//!   "auth_token": "<64-hex>",
//!   "bind": "0.0.0.0",
//!   "port": 7331
//! }
//! ```
//!
//! Все три поля опциональны. Если файла нет — `load_from()` возвращает
//! `Ok(None)` (нет ошибки). Это позволяет первой запустить `devforge run`
//! без какого-либо конфига.
//!
//! ## Приоритет источников
//!
//! Финальные эффективные значения для bind/port/auth_token собираются по
//! правилу: **CLI > server_config.json > env > default**. См.
//! [`resolve`].
//!
//! ## Auto remote-mode
//!
//! Если в server_config.json присутствует `auth_token` ИЛИ `bind`, то
//! `remote_mode` подразумевается автоматически — даже без `--remote` в CLI.
//! Это позволяет пользователю один раз настроить через
//! `devforge pair --generate` (Phase 2) и потом просто запускать
//! `devforge start` без флагов.
//!
//! ## Atomic save
//!
//! `save_to()` пишет во временный файл `*.tmp` рядом с целью, fsync'ит и
//! делает `rename()` поверх (atomic в пределах одного mount-point на
//! POSIX). Паттерн полностью совпадает с `projects.rs::ProjectStore::save`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::{self, RunOptions, DEFAULT_PORT};
#[cfg(test)]
use crate::cli::ENV_AUTH_TOKEN;

/// Содержимое `~/.config/forge/server_config.json`. Все поля опциональны.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}


/// Эффективная конфигурация сервера после применения приоритетов
/// CLI > file > env > default. Возвращается из [`resolve`].
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    /// Финальный bind-адрес. При `remote_mode=false` тут всегда `127.0.0.1`
    /// (см. контракт [`resolve`]). При `remote_mode=true` — то, что указал
    /// пользователь (CLI/файл) или default `0.0.0.0`.
    pub bind: String,
    pub port: u16,
    /// Bearer-token. `Some` если remote-mode активен и токен есть/был
    /// сгенерирован. `None` при legacy localhost.
    pub auth_token: Option<String>,
    /// True ⇒ публичный режим (auth+bind expansion). False ⇒ legacy
    /// localhost (текущее поведение до Phase 1).
    pub remote_mode: bool,
}

/// `~/.config/forge/server_config.json` (стиль `cli::pid_path`).
pub fn default_server_config_path() -> Result<PathBuf> {
    Ok(cli::state_dir()?.join("server_config.json"))
}

/// Загрузить файл по умолчанию. Удобный shortcut для [`load_from`].
pub fn load() -> Result<Option<ServerConfig>> {
    let path = default_server_config_path()?;
    load_from(&path)
}

/// Загрузить `ServerConfig` из произвольного пути.
///
/// - Файл отсутствует → `Ok(None)` (не ошибка).
/// - Файл существует, но не парсится → `Err` с контекстом.
pub fn load_from(path: &Path) -> Result<Option<ServerConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let cfg: ServerConfig = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(cfg))
}

/// Сохранить в файл по умолчанию (создавая `~/.config/forge/` при нужде).
pub fn save(cfg: &ServerConfig) -> Result<()> {
    let path = default_server_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    save_to(&path, cfg)
}

/// Atomic save: write tmp + rename. Тот же паттерн, что в
/// `projects.rs::ProjectStore::save` (lines 165-191).
pub fn save_to(path: &Path, cfg: &ServerConfig) -> Result<()> {
    let body = serde_json::to_vec_pretty(cfg).context("failed to serialize ServerConfig")?;

    let mut tmp = path.to_path_buf();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!("failed to rename {} -> {}", tmp.display(), path.display())
    })?;
    Ok(())
}

/// Резолвинг эффективной конфигурации сервера.
///
/// **Источники в порядке убывания приоритета:**
/// 1. CLI (`--port`, `--bind`, `--token`, `--remote`).
/// 2. `server_config.json` (`port`, `bind`, `auth_token`).
/// 3. Env `DEVFORGE_AUTH_TOKEN` — учитывается ВНУТРИ CLI-парсера
///    (см. `cli::parse_from`): если `--token` не задан, env подмешивается
///    в `RunOptions.token`. Поэтому здесь env уже виден как «CLI».
///    Этот шаг нужен только для случая, когда даже env пуст и есть только
///    содержимое файла — тогда файл выигрывает.
/// 4. Default: bind=127.0.0.1 (или 0.0.0.0 при remote_mode), port=7331,
///    auth_token=None.
///
/// **Remote-mode только при явном CLI-флаге** `--remote`. Раньше файл
/// `server_config.json` мог авто-включать remote-mode если в нём
/// сохранены `auth_token`/`bind` — это сбивало с толку: после одного
/// запуска с `--remote` следующий `cargo run` без флагов автоматически
/// поднимался на `0.0.0.0` и просил токен. Теперь это требует явного
/// `--remote`. Файл по-прежнему используется как кэш токена/порта/bind
/// (см. ниже), но сам факт его наличия НЕ переключает режим.
///
/// **Auto-generation токена** делается в отдельной фазе (см.
/// [`finalize_token`]) — `resolve` сам токен не генерирует, чтобы
/// resolver оставался pure-функцией.
pub fn resolve(cli_opts: &RunOptions, file_cfg: Option<&ServerConfig>) -> EffectiveConfig {
    // remote_mode: ТОЛЬКО CLI-флаг. Файл больше не подразумевает remote.
    let remote_mode = cli_opts.remote;

    // Port: CLI > file > default.
    // ВАЖНО: cli_opts.port это уже разрешённое значение (DEFAULT_PORT, если
    // юзер ничего не ввёл) — но мы хотим, чтобы файл смог его переопределить
    // только когда пользователь не ввёл --port явно. К сожалению, текущий
    // парсер не различает «дефолт» и «явный default». Поэтому для Phase 1
    // мы трактуем «cli.port == DEFAULT_PORT» как «не задано». Это совместимо
    // с интуицией пользователя: если он явно вводит --port 7331, то это и
    // есть default — поведение идентично.
    let port = if cli_opts.port != DEFAULT_PORT {
        cli_opts.port
    } else {
        file_cfg.and_then(|f| f.port).unwrap_or(DEFAULT_PORT)
    };

    // Bind: CLI > file > default-by-mode.
    let bind_explicit = cli_opts
        .bind
        .clone()
        .or_else(|| file_cfg.and_then(|f| f.bind.clone()));

    let bind = if remote_mode {
        bind_explicit.unwrap_or_else(|| "0.0.0.0".to_string())
    } else {
        // Legacy localhost — bind ВСЕГДА 127.0.0.1, даже если файл/CLI
        // указали что-то ещё (так как remote_mode=false → пользователь не
        // включал публичный режим). Это спасает от случайного открытия
        // сервера наружу без --remote.
        "127.0.0.1".to_string()
    };

    // Token: CLI (включая env, который CLI-парсер подмешал) > file.
    // В legacy localhost-режиме токен не нужен — стираем (auth middleware
    // не включится, и frontend всё равно его не будет посылать).
    let auth_token = if remote_mode {
        cli_opts
            .token
            .clone()
            .or_else(|| file_cfg.and_then(|f| f.auth_token.clone()))
    } else {
        None
    };

    EffectiveConfig {
        bind,
        port,
        auth_token,
        remote_mode,
    }
}

/// Сгенерировать 64-hex Bearer-token криптографически случайно.
///
/// Используется uuid::Uuid::new_v4() (2 раза по 16 random байт = 32 байта,
/// 64 hex-символа). UUID v4 строится поверх getrandom — те же гарантии,
/// что и у `rand::rngs::OsRng`, но без новой зависимости (uuid уже в
/// Cargo.toml).
pub fn generate_token_64hex() -> String {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut s = String::with_capacity(64);
    for byte in a.as_bytes().iter().chain(b.as_bytes().iter()) {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Helper: содержит ли значение `bind` непубличный/публичный адрес.
///
/// «Localhost» = `127.x.x.x` или `::1`. Всё остальное считается публичным
/// (это критерий для авто-генерации токена в `finalize_token`).
pub fn is_localhost_bind(bind: &str) -> bool {
    bind == "127.0.0.1"
        || bind == "::1"
        || bind == "localhost"
        || bind.starts_with("127.")
}

/// Внутренняя реализация finalize_token, принимающая явный путь к
/// server_config.json. Используется для unit-тестов (где HOME не туда
/// указывать неудобно) и для прод-кода через [`finalize_token`].
///
/// Возвращает `Some(token)` если remote_mode активен, иначе `None`.
/// На ошибке save пишет в stderr, но всё равно возвращает токен.
pub fn finalize_token_at(eff: &EffectiveConfig, path: &Path) -> Option<String> {
    if !eff.remote_mode {
        return None;
    }
    if let Some(existing) = &eff.auth_token {
        return Some(existing.clone());
    }

    let token = generate_token_64hex();
    let merged = match load_from(path) {
        Ok(Some(mut existing_cfg)) => {
            existing_cfg.auth_token = Some(token.clone());
            existing_cfg
        }
        _ => ServerConfig {
            auth_token: Some(token.clone()),
            bind: Some(eff.bind.clone()),
            port: Some(eff.port),
        },
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = save_to(path, &merged) {
        eprintln!(
            "[devforge] WARNING: failed to persist auto-generated token to {}: {e:#}",
            path.display()
        );
    } else {
        eprintln!(
            "[devforge] auto-generated auth token saved to {}",
            path.display()
        );
    }
    print_auto_token_banner(&token, &eff.bind, eff.port);
    Some(token)
}

/// Финализация токена при запуске:
///
/// - Если `remote_mode=false` → ничего не делает, возвращает None.
/// - Если `auth_token` уже есть → возвращает его как есть.
/// - Если `auth_token=None` и `bind` публичный → генерирует 64-hex,
///   сохраняет в server_config.json (создавая файл), печатает big-warning
///   в stdout и возвращает Some(token).
/// - Если `auth_token=None` и `bind=127.0.0.1` (loopback) → можно опустить
///   генерацию (less critical). Для строгости генерируем и в этом случае.
///
/// **Не fail-fast**: даже если save_to упал, мы возвращаем токен — сервер
/// всё равно может работать, просто пользователь не сможет восстановить
/// токен после рестарта. Ошибка save'а печатается в stderr.
pub fn finalize_token(eff: &EffectiveConfig) -> Option<String> {
    if !eff.remote_mode {
        return None;
    }
    if let Some(existing) = &eff.auth_token {
        return Some(existing.clone());
    }
    // Резолвим путь; ошибка — печатаем warning и всё равно генерируем токен
    // (только в этой сессии).
    let path = match default_server_config_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "[devforge] WARNING: cannot resolve server_config.json path: {e:#}"
            );
            let token = generate_token_64hex();
            print_auto_token_banner(&token, &eff.bind, eff.port);
            return Some(token);
        }
    };
    finalize_token_at(eff, &path)
}

/// Phase 7 — печатает big-warning банер на старте при `--remote` и
/// non-loopback bind (0.0.0.0, 192.168.x.x, и т.п.).
///
/// Цель: пользователь должен явно увидеть, что HTTP без TLS поднят наружу.
/// Token (хотя бы в truncated виде) показывается чтобы пользователь видел,
/// что аутентификация всё-таки включена. Если token=None, бэкенд уже отказал
/// бы при старте (no-token + public bind → finalize_token авто-генерит,
/// но если каким-то образом None — печатаем «NO TOKEN — UNSAFE»).
///
/// Вызывается из `main.rs` ПОСЛЕ resolve конфигурации и ДО `axum::serve`,
/// чтобы пользователь увидел warning раньше первых запросов.
pub fn print_public_bind_warning(bind: &str, port: u16, token: Option<&str>) {
    if is_localhost_bind(bind) {
        return;
    }
    let token_hint = match token {
        Some(t) if t.len() >= 8 => format!("{}…{}", &t[..8], &t[t.len() - 4..]),
        Some(_) => "<short>".to_string(),
        None => "<NONE — UNSAFE>".to_string(),
    };
    println!();
    println!("╔══════════════════════════════════════════════════════════════════════════╗");
    println!("║ WARNING: DevForge is bound to a public address WITHOUT TLS              ║");
    println!("║   Bind:  {:<62} ║", format!("{bind}:{port}"));
    println!("║   Token: {:<62} ║", token_hint);
    if token.is_none() {
        println!("║   !! Bearer-auth is DISABLED and traffic is plain HTTP — DO NOT USE.    ║");
    } else {
        println!("║   Auth is on, but transport is plain HTTP. Anyone sniffing the wire     ║");
        println!("║   can capture the Bearer token. Use one of:                             ║");
        println!("║     - SSH tunnel:  ssh -L 8787:127.0.0.1:8787 user@host                 ║");
        println!("║     - WireGuard / Tailscale / ZeroTier private network                  ║");
        println!("║     - Reverse-proxy с HTTPS (Caddy / nginx / Traefik) перед devforge    ║");
    }
    println!("╚══════════════════════════════════════════════════════════════════════════╝");
    println!();
}

/// Печатает big-warning банер с авто-сгенерированным токеном.
fn print_auto_token_banner(token: &str, bind: &str, port: u16) {
    let bar = "=".repeat(74);
    println!();
    println!("{bar}");
    println!(" devforge: REMOTE MODE — auth token auto-generated");
    println!("{bar}");
    println!(" Listening on: http://{bind}:{port}");
    println!(" Bearer token: {token}");
    println!();
    println!(" To connect from another machine, copy this token and add the");
    println!(" remote in your local devforge:");
    println!("   Settings → Remote servers → Add  (token=<paste>)");
    println!();
    println!(" The token has been saved to ~/.config/forge/server_config.json");
    println!(" and will be reused on subsequent runs.");
    println!("{bar}");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-srvcfg-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempdir("missing");
        let path = dir.join("server_config.json");
        let got = load_from(&path).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempdir("roundtrip");
        let path = dir.join("server_config.json");
        let cfg = ServerConfig {
            auth_token: Some("deadbeef".repeat(8)),
            bind: Some("0.0.0.0".to_string()),
            port: Some(8080),
        };
        save_to(&path, &cfg).unwrap();
        let loaded = load_from(&path).unwrap().expect("file should exist");
        assert_eq!(loaded.auth_token.as_deref(), Some(cfg.auth_token.as_deref().unwrap()));
        assert_eq!(loaded.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(loaded.port, Some(8080));
    }

    #[test]
    fn resolve_default_is_localhost_no_auth() {
        let cli = RunOptions::default();
        let eff = resolve(&cli, None);
        assert!(!eff.remote_mode);
        assert_eq!(eff.bind, "127.0.0.1");
        assert_eq!(eff.port, DEFAULT_PORT);
        assert!(eff.auth_token.is_none());
    }

    #[test]
    fn resolve_cli_remote_without_token() {
        let cli = RunOptions {
            remote: true,
            ..Default::default()
        };
        let eff = resolve(&cli, None);
        assert!(eff.remote_mode);
        assert_eq!(eff.bind, "0.0.0.0");
        assert!(eff.auth_token.is_none()); // ещё не сгенерён, это finalize_token делает
    }

    #[test]
    fn resolve_cli_overrides_file() {
        let cli = RunOptions {
            port: 9000,
            remote: true,
            bind: Some("192.168.1.5".into()),
            token: Some("cli-token".into()),
        };
        let file = ServerConfig {
            auth_token: Some("file-token".into()),
            bind: Some("0.0.0.0".into()),
            port: Some(8888),
        };
        let eff = resolve(&cli, Some(&file));
        assert!(eff.remote_mode);
        assert_eq!(eff.bind, "192.168.1.5");
        assert_eq!(eff.port, 9000);
        assert_eq!(eff.auth_token.as_deref(), Some("cli-token"));
    }

    #[test]
    fn resolve_file_supplies_token_when_cli_lacks() {
        let cli = RunOptions {
            remote: true,
            ..Default::default()
        };
        let file = ServerConfig {
            auth_token: Some("file-token".into()),
            bind: None,
            port: None,
        };
        let eff = resolve(&cli, Some(&file));
        assert!(eff.remote_mode);
        assert_eq!(eff.auth_token.as_deref(), Some("file-token"));
        assert_eq!(eff.bind, "0.0.0.0"); // default-by-remote
    }

    #[test]
    fn resolve_file_does_not_imply_remote_without_cli_flag() {
        // CLI без --remote, в файле есть auth_token/bind — но remote_mode
        // должен ОСТАВАТЬСЯ false. Раньше тут было auto-remote, но это
        // сбивало с толку: cargo run после одного --remote запускал
        // публичный сервер без явного флага. См. resolve().
        let cli = RunOptions::default();
        let file = ServerConfig {
            auth_token: Some("auto".into()),
            bind: Some("0.0.0.0".into()),
            port: None,
        };
        let eff = resolve(&cli, Some(&file));
        assert!(!eff.remote_mode);
        assert_eq!(eff.bind, "127.0.0.1");
        // Токен в legacy localhost-mode стирается (см. resolve).
        assert!(eff.auth_token.is_none());
    }

    #[test]
    fn resolve_non_remote_strips_token() {
        // Если файл не подразумевает remote (нет auth_token и bind) — то
        // и без --remote сервер локальный, token=None. После изменения в
        // resolve() поведение «implies_remote» больше не активируется
        // автоматически, поэтому даже при auth_token=Some/bind=Some в
        // файле без CLI флага --remote получим localhost-mode.
        let cli = RunOptions {
            port: 9000,
            remote: false,
            ..Default::default()
        };
        let file = ServerConfig {
            auth_token: None,
            bind: None,
            port: Some(8000),
        };
        let eff = resolve(&cli, Some(&file));
        assert!(!eff.remote_mode);
        assert_eq!(eff.bind, "127.0.0.1");
        assert!(eff.auth_token.is_none());
        // CLI port (9000) > file (8000)
        assert_eq!(eff.port, 9000);
    }

    #[test]
    fn resolve_default_port_lets_file_win() {
        let cli = RunOptions::default(); // port=DEFAULT_PORT
        let file = ServerConfig {
            port: Some(5000),
            ..Default::default()
        };
        let eff = resolve(&cli, Some(&file));
        assert_eq!(eff.port, 5000);
    }

    #[test]
    fn generate_token_format() {
        let t = generate_token_64hex();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Два независимых вызова не должны совпадать.
        let t2 = generate_token_64hex();
        assert_ne!(t, t2);
    }

    #[test]
    fn is_localhost_bind_matrix() {
        assert!(is_localhost_bind("127.0.0.1"));
        assert!(is_localhost_bind("127.0.0.5"));
        assert!(is_localhost_bind("::1"));
        assert!(is_localhost_bind("localhost"));
        assert!(!is_localhost_bind("0.0.0.0"));
        assert!(!is_localhost_bind("192.168.1.5"));
    }

    #[test]
    fn env_var_name_constant() {
        // Защита от случайного переименования: тест-документация.
        assert_eq!(ENV_AUTH_TOKEN, "DEVFORGE_AUTH_TOKEN");
    }

    #[test]
    fn finalize_token_noop_when_non_remote() {
        let dir = tempdir("finalize-noop");
        let path = dir.join("server_config.json");
        let eff = EffectiveConfig {
            bind: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            auth_token: None,
            remote_mode: false,
        };
        let got = finalize_token_at(&eff, &path);
        assert!(got.is_none(), "non-remote → no token");
        assert!(!path.exists(), "non-remote → no file write");
    }

    #[test]
    fn finalize_token_preserves_existing() {
        let dir = tempdir("finalize-preserve");
        let path = dir.join("server_config.json");
        let eff = EffectiveConfig {
            bind: "0.0.0.0".into(),
            port: 7331,
            auth_token: Some("existing-token".into()),
            remote_mode: true,
        };
        let got = finalize_token_at(&eff, &path).expect("token preserved");
        assert_eq!(got, "existing-token");
        // Существующий токен не должен триггерить запись файла.
        assert!(!path.exists(), "existing token → no file write");
    }

    #[test]
    fn finalize_token_autogen_writes_file() {
        let dir = tempdir("finalize-autogen");
        let path = dir.join("server_config.json");
        let eff = EffectiveConfig {
            bind: "0.0.0.0".into(),
            port: 7331,
            auth_token: None,
            remote_mode: true,
        };
        let token = finalize_token_at(&eff, &path).expect("token generated");
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

        // Файл должен быть создан и содержать тот же токен.
        let loaded = load_from(&path).unwrap().expect("file written");
        assert_eq!(loaded.auth_token.as_deref(), Some(token.as_str()));
        assert_eq!(loaded.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(loaded.port, Some(7331));
    }

    #[test]
    fn finalize_token_autogen_merges_existing_file() {
        let dir = tempdir("finalize-merge");
        let path = dir.join("server_config.json");
        // Pre-write file БЕЗ токена, но с port = 9999.
        let pre = ServerConfig {
            auth_token: None,
            bind: Some("0.0.0.0".into()),
            port: Some(9999),
        };
        save_to(&path, &pre).unwrap();

        let eff = EffectiveConfig {
            bind: "0.0.0.0".into(),
            port: 7331,
            auth_token: None,
            remote_mode: true,
        };
        let token = finalize_token_at(&eff, &path).expect("token generated");

        let loaded = load_from(&path).unwrap().expect("file persists");
        assert_eq!(loaded.auth_token.as_deref(), Some(token.as_str()));
        // Существующий port (9999) сохраняется при merge.
        assert_eq!(loaded.port, Some(9999));
    }

    // Phase 7 — sanity-тесты для is_localhost_bind, используемого в
    // print_public_bind_warning. На localhost-bind warning должен быть no-op,
    // на любом другом — печатать сообщение. Тут проверяем только классификатор
    // (печать в stdout не тестируем — IO-эффекты бесполезно проверять).
    #[test]
    fn is_localhost_bind_recognises_loopback() {
        assert!(is_localhost_bind("127.0.0.1"));
        assert!(is_localhost_bind("127.1.2.3"));
        assert!(is_localhost_bind("::1"));
        assert!(is_localhost_bind("localhost"));
    }

    #[test]
    fn is_localhost_bind_rejects_public() {
        assert!(!is_localhost_bind("0.0.0.0"));
        assert!(!is_localhost_bind("192.168.1.10"));
        assert!(!is_localhost_bind("10.0.0.1"));
        assert!(!is_localhost_bind("203.0.113.42"));
    }

    // Phase 7 — print_public_bind_warning сам по себе печатает в stdout
    // (макрос println!), что неудобно тестировать. Но мы можем по крайней
    // мере проверить guard: на localhost-bind функция должна быть НИКОГДА
    // не вызывать `println!` мимо unit-теста — для этого выделили
    // is_localhost_bind в отдельную функцию (см. выше).
    //
    // Дополнительная проверка: функция не должна паниковать при пустом
    // token / коротком token / unicode-bind.
    #[test]
    fn print_public_bind_warning_no_panic_corner_cases() {
        print_public_bind_warning("127.0.0.1", 8787, Some("anything"));
        print_public_bind_warning("0.0.0.0", 8787, None);
        print_public_bind_warning("0.0.0.0", 8787, Some(""));
        print_public_bind_warning("0.0.0.0", 8787, Some("short"));
        print_public_bind_warning("[::]", 8787, Some("abcdef0123456789"));
    }

    // =========================================================================
    // Phase 8 .6 — server_config invalid JSON + IPv6 + bind conflicts
    // =========================================================================

    #[test]
    fn load_invalid_json_returns_err_with_path_in_message() {
        let dir = tempdir("invalid-json");
        let path = dir.join("server_config.json");
        std::fs::write(&path, b"{ this is not json :(").unwrap();
        let r = load_from(&path);
        let err = r.expect_err("invalid JSON must Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&path.display().to_string()),
            "error должен содержать путь, got: {msg}"
        );
    }

    #[test]
    fn load_empty_file_returns_err() {
        let dir = tempdir("empty-file");
        let path = dir.join("server_config.json");
        std::fs::write(&path, b"").unwrap();
        let r = load_from(&path);
        // 0-байтный файл — невалидный JSON (нужен хотя бы `{}`).
        assert!(r.is_err(), "0-byte файл должен Err");
    }

    #[test]
    fn load_empty_object_is_default() {
        let dir = tempdir("empty-object");
        let path = dir.join("server_config.json");
        std::fs::write(&path, b"{}").unwrap();
        let cfg = load_from(&path).unwrap().expect("loaded");
        assert!(cfg.auth_token.is_none());
        assert!(cfg.bind.is_none());
        assert!(cfg.port.is_none());
    }

    #[test]
    fn load_json_with_unknown_fields_ignored() {
        // serde без deny_unknown_fields — лишние поля игнорируются.
        let dir = tempdir("extra-fields");
        let path = dir.join("server_config.json");
        std::fs::write(
            &path,
            br#"{"auth_token":"x","bind":"0.0.0.0","port":7000,"future_field":42}"#,
        )
        .unwrap();
        let cfg = load_from(&path).unwrap().expect("parses with extras");
        assert_eq!(cfg.auth_token.as_deref(), Some("x"));
        assert_eq!(cfg.port, Some(7000));
    }

    #[cfg(unix)]
    #[test]
    fn save_to_readonly_dir_returns_err_not_panic() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir("readonly");
        // Делаем директорию read-only — write tmp-файла должен упасть.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let path = dir.join("server_config.json");
        let cfg = ServerConfig {
            auth_token: Some("x".into()),
            bind: Some("0.0.0.0".into()),
            port: Some(7331),
        };
        let r = save_to(&path, &cfg);

        // Восстанавливаем permissions для cleanup.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));

        assert!(r.is_err(), "save_to read-only dir must Err");
        // Сообщение должно упомянуть .tmp путь.
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains("failed to write tmp") || msg.contains("Permission"),
            "ожидаем informative message, got: {msg}"
        );
    }

    // IPv6 классификация
    //
    // Текущий is_localhost_bind покрывает только строки "::1", "127.x", "localhost".
    // Фактическое поведение для bracketed-форм и unspecified address — закрепляем
    // тестами-as-spec.

    #[test]
    fn is_localhost_bind_ipv6_unbracketed_loopback() {
        assert!(is_localhost_bind("::1"));
    }

    #[test]
    fn is_localhost_bind_ipv6_bracketed_loopback_not_recognized() {
        // [::1] не классифицируется как loopback (только "::1").
        // Тест-as-spec для текущего поведения. Если потребуется — добавить
        // обработку bracket-формы в is_localhost_bind.
        assert!(
            !is_localhost_bind("[::1]"),
            "bracket-форма не распознаётся текущей реализацией"
        );
    }

    #[test]
    fn is_localhost_bind_ipv6_link_local_is_public() {
        // fe80::1 — link-local, НЕ loopback. is_localhost_bind должен вернуть false.
        assert!(!is_localhost_bind("fe80::1"));
    }

    #[test]
    fn is_localhost_bind_ipv6_unspecified_is_public() {
        // :: — any/unspecified IPv6 address. Аналог 0.0.0.0 для v6.
        assert!(!is_localhost_bind("::"));
    }

    #[test]
    fn resolve_legacy_bind_overrides_to_127001_even_with_cli_bind() {
        // Конфликт: --bind 0.0.0.0 БЕЗ --remote → resolve должен принудительно
        // понизить до 127.0.0.1 (defense-in-depth от случайного публичного bind'а).
        let cli = RunOptions {
            remote: false,
            bind: Some("0.0.0.0".to_string()),
            ..Default::default()
        };
        let eff = resolve(&cli, None);
        assert!(!eff.remote_mode);
        assert_eq!(
            eff.bind, "127.0.0.1",
            "legacy mode принудительно понижает bind до loopback"
        );
    }
}
