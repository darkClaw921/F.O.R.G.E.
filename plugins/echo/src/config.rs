//! Конфигурация плагина Echo.
//!
//! [`EchoConfig`] заменяет `EchoConfigStub` (Phase 1-5) — полноценная
//! структура со всеми параметрами:
//!
//! - `cli_path` — путь к Claude CLI (`~/.local/bin/claude` по дефолту, либо
//!   `claude` если HOME не задан — резолв через PATH).
//! - `db_path` — путь к SQLite (`~/.config/forge/echo.db`).
//! - `max_parallel_runs` — лимит одновременных Claude-run'ов (4).
//! - `default_model` — модель по умолчанию (`claude-3-5-sonnet-latest`).
//! - `capture_lines` — сколько строк tmux capture-pane вставлять в prompt (200).
//! - `autonomous_max_tokens_per_day` — дневной cap токенов для автономных
//!   задач (200_000); при превышении задачи авто-отключаются и пользователь
//!   получает notification.
//!
//! ## Источники
//!
//! 1. [`EchoConfig::default`] — статические дефолты.
//! 2. [`EchoConfig::load_from_file`] — читает `~/.config/forge/echo.toml` если
//!    он существует; missing / битый TOML → warn-log + дефолты.
//! 3. [`EchoConfig::apply_env`] — env-override через `FORGE_ECHO_*`:
//!    - `FORGE_ECHO_CLI_PATH`
//!    - `FORGE_ECHO_DB_PATH`
//!    - `FORGE_ECHO_MAX_PARALLEL_RUNS`
//!    - `FORGE_ECHO_DEFAULT_MODEL`
//!    - `FORGE_ECHO_CAPTURE_LINES`
//!    - `FORGE_ECHO_AUTONOMOUS_MAX_TOKENS_PER_DAY`
//!
//! [`EchoConfig::load`] делает всё вместе: defaults → файл → env.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Дефолтный путь TOML-конфига плагина.
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/echo.toml")
    } else {
        PathBuf::from("./echo.toml")
    }
}

/// Дефолтный путь к SQLite-БД плагина.
pub fn default_db_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/echo.db")
    } else {
        PathBuf::from("./echo.db")
    }
}

/// Дефолтный путь к Claude CLI. Если HOME не задан — fallback `claude` (PATH).
pub fn default_cli_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/bin/claude")
    } else {
        PathBuf::from("claude")
    }
}

/// Дефолтная модель Claude. На момент Phase 6 — последний стабильный Sonnet
/// 3.5 alias. UI/пользователь может переопределить per-conversation.
pub const DEFAULT_MODEL: &str = "claude-3-5-sonnet-latest";

/// Дефолтное количество строк capture-pane, вшиваемых в prompt.
pub const DEFAULT_CAPTURE_LINES: i32 = 200;

/// Дефолтный дневной cap токенов для автономных задач. 200K — достаточно
/// для нескольких десятков самостоятельных запусков с типовыми prompt'ами,
/// но защищает от runaway-конфигураций.
pub const DEFAULT_AUTONOMOUS_MAX_TOKENS_PER_DAY: u64 = 200_000;

/// Дефолтный rate-limit на user_message — 30 сообщений в минуту на WS.
pub const DEFAULT_USER_MESSAGE_RATE_LIMIT_PER_MIN: u32 = 30;

/// Полная конфигурация плагина Echo.
///
/// Cheap-clonable (поля по значению). Передаётся в [`crate::init`] вместо
/// `EchoConfigStub`. Тесты могут собирать `EchoConfig::default()` и
/// переопределять отдельные поля; продакшен — [`EchoConfig::load`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoConfig {
    /// Путь к Claude CLI. Если файл не существует на момент `init`, runner
    /// печатает warn-log и продолжает (для healthz/тестов).
    #[serde(default = "default_cli_path")]
    pub cli_path: PathBuf,
    /// Путь к SQLite-БД плагина (создаётся при первом запуске).
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    /// Сколько одновременных Claude-run'ов разрешено (Semaphore lim).
    /// Минимум 1; значения < 1 в config'е валидируются в `validate`.
    #[serde(default = "default_max_parallel_runs")]
    pub max_parallel_runs: usize,
    /// Дефолтная модель.
    #[serde(default = "default_model_string")]
    pub default_model: String,
    /// Сколько строк capture-pane вставлять в prompt по умолчанию.
    #[serde(default = "default_capture_lines")]
    pub capture_lines: i32,
    /// Дневной cap токенов для autonomous-задач (Phase 6 hardening).
    /// 0 → cap отключен.
    #[serde(default = "default_autonomous_cap")]
    pub autonomous_max_tokens_per_day: u64,
    /// Rate-limit на user_message (per WS, sliding 60s).
    /// 0 → лимит отключен (используется в тестах).
    #[serde(default = "default_user_message_rate_limit")]
    pub user_message_rate_limit_per_min: u32,
}

fn default_max_parallel_runs() -> usize {
    4
}

fn default_model_string() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_capture_lines() -> i32 {
    DEFAULT_CAPTURE_LINES
}

fn default_autonomous_cap() -> u64 {
    DEFAULT_AUTONOMOUS_MAX_TOKENS_PER_DAY
}

fn default_user_message_rate_limit() -> u32 {
    DEFAULT_USER_MESSAGE_RATE_LIMIT_PER_MIN
}

impl Default for EchoConfig {
    fn default() -> Self {
        Self {
            cli_path: default_cli_path(),
            db_path: default_db_path(),
            max_parallel_runs: default_max_parallel_runs(),
            default_model: default_model_string(),
            capture_lines: default_capture_lines(),
            autonomous_max_tokens_per_day: default_autonomous_cap(),
            user_message_rate_limit_per_min: default_user_message_rate_limit(),
        }
    }
}

impl EchoConfig {
    /// Полная загрузка: defaults → optional TOML-файл → env-override.
    ///
    /// Эта функция никогда не падает — все ошибки (missing file, parse error,
    /// invalid env value) трактуются как «использовать дефолт» с warn-логом.
    pub fn load() -> Self {
        let mut cfg = Self::load_from_file(&default_config_path());
        cfg.apply_env();
        cfg.validate_and_fix();
        cfg
    }

    /// Загружает конфиг из TOML-файла. Если файла нет — возвращает дефолт.
    /// Если файл битый — печатает warn-лог и возвращает дефолт.
    pub fn load_from_file(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(body) => match toml::from_str::<EchoConfig>(&body) {
                Ok(c) => {
                    tracing::info!(path = %path.display(), "forge-echo: loaded config from TOML");
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "forge-echo: failed to parse echo.toml; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %path.display(),
                    "forge-echo: echo.toml not found, using defaults"
                );
                Self::default()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "forge-echo: failed to read echo.toml; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Применяет env-override `FORGE_ECHO_*`. Невалидные числа → warn + skip.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FORGE_ECHO_CLI_PATH") {
            if !v.is_empty() {
                self.cli_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_DB_PATH") {
            if !v.is_empty() {
                self.db_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_MAX_PARALLEL_RUNS") {
            match v.parse::<usize>() {
                Ok(n) if n >= 1 => self.max_parallel_runs = n,
                _ => tracing::warn!(value = %v, "FORGE_ECHO_MAX_PARALLEL_RUNS: invalid (need >=1), keeping {}", self.max_parallel_runs),
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_DEFAULT_MODEL") {
            if !v.is_empty() {
                self.default_model = v;
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_CAPTURE_LINES") {
            match v.parse::<i32>() {
                Ok(n) if n > 0 => self.capture_lines = n,
                _ => tracing::warn!(value = %v, "FORGE_ECHO_CAPTURE_LINES: invalid (need >0), keeping {}", self.capture_lines),
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_AUTONOMOUS_MAX_TOKENS_PER_DAY") {
            match v.parse::<u64>() {
                Ok(n) => self.autonomous_max_tokens_per_day = n,
                Err(_) => tracing::warn!(value = %v, "FORGE_ECHO_AUTONOMOUS_MAX_TOKENS_PER_DAY: not a u64, keeping {}", self.autonomous_max_tokens_per_day),
            }
        }
        if let Ok(v) = std::env::var("FORGE_ECHO_USER_MESSAGE_RATE_LIMIT_PER_MIN") {
            match v.parse::<u32>() {
                Ok(n) => self.user_message_rate_limit_per_min = n,
                Err(_) => tracing::warn!(value = %v, "FORGE_ECHO_USER_MESSAGE_RATE_LIMIT_PER_MIN: not a u32, keeping {}", self.user_message_rate_limit_per_min),
            }
        }
    }

    /// Принудительно поднимает поля до минимальных валидных значений.
    /// Вызывается после `load_from_file` + `apply_env` чтобы гарантировать
    /// что runner получит работающий конфиг даже при «творческих» правках.
    pub fn validate_and_fix(&mut self) {
        if self.max_parallel_runs < 1 {
            tracing::warn!("forge-echo: max_parallel_runs<1, raising to 1");
            self.max_parallel_runs = 1;
        }
        if self.capture_lines <= 0 {
            tracing::warn!("forge-echo: capture_lines<=0, raising to 200");
            self.capture_lines = DEFAULT_CAPTURE_LINES;
        }
        if self.default_model.is_empty() {
            tracing::warn!("forge-echo: default_model empty, raising to {DEFAULT_MODEL}");
            self.default_model = DEFAULT_MODEL.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper для tempfile-конфига без зависимости на tempfile (есть в dev-deps).
    fn write_tmp_toml(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn default_has_sensible_values() {
        let c = EchoConfig::default();
        assert!(c.max_parallel_runs >= 1);
        assert!(c.capture_lines > 0);
        assert!(!c.default_model.is_empty());
        assert_eq!(c.autonomous_max_tokens_per_day, 200_000);
        assert_eq!(c.user_message_rate_limit_per_min, 30);
    }

    #[test]
    fn load_from_file_returns_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let c = EchoConfig::load_from_file(&path);
        assert_eq!(c.max_parallel_runs, 4);
        assert_eq!(c.default_model, DEFAULT_MODEL);
    }

    #[test]
    fn load_from_file_parses_partial_toml() {
        // Только два поля — остальные через #[serde(default)] должны взяться.
        let body = r#"
            max_parallel_runs = 8
            default_model = "claude-opus-test"
        "#;
        let f = write_tmp_toml(body);
        let c = EchoConfig::load_from_file(f.path());
        assert_eq!(c.max_parallel_runs, 8);
        assert_eq!(c.default_model, "claude-opus-test");
        assert_eq!(c.capture_lines, DEFAULT_CAPTURE_LINES);
        assert_eq!(c.autonomous_max_tokens_per_day, DEFAULT_AUTONOMOUS_MAX_TOKENS_PER_DAY);
    }

    #[test]
    fn load_from_file_returns_default_when_malformed() {
        let f = write_tmp_toml("this is not toml = = = !!!\n[ unclosed");
        let c = EchoConfig::load_from_file(f.path());
        // Не упал, дал дефолты.
        assert_eq!(c.max_parallel_runs, 4);
    }

    // ⚠️ Тесты env-override объединены в один — `std::env::set_var` глобален
    // для процесса, и параллельный запуск тестов приводит к race-condition
    // (cargo test gонит unit-тесты в пуле потоков по умолчанию).
    #[test]
    fn apply_env_overrides_and_invalid_values() {
        // Случай 1: валидные override'ы применяются полностью.
        let mut c = EchoConfig::default();
        std::env::set_var("FORGE_ECHO_DEFAULT_MODEL", "override-test-model-xyz");
        std::env::set_var("FORGE_ECHO_MAX_PARALLEL_RUNS", "12");
        std::env::set_var("FORGE_ECHO_CAPTURE_LINES", "500");
        std::env::set_var("FORGE_ECHO_AUTONOMOUS_MAX_TOKENS_PER_DAY", "1234567");
        std::env::set_var("FORGE_ECHO_USER_MESSAGE_RATE_LIMIT_PER_MIN", "100");
        c.apply_env();
        assert_eq!(c.default_model, "override-test-model-xyz");
        assert_eq!(c.max_parallel_runs, 12);
        assert_eq!(c.capture_lines, 500);
        assert_eq!(c.autonomous_max_tokens_per_day, 1_234_567);
        assert_eq!(c.user_message_rate_limit_per_min, 100);

        // Случай 2: невалидные числа не меняют поля (warn + skip).
        std::env::set_var("FORGE_ECHO_MAX_PARALLEL_RUNS", "not-a-number");
        std::env::set_var("FORGE_ECHO_CAPTURE_LINES", "-1");
        let before_pr = c.max_parallel_runs;
        let before_cl = c.capture_lines;
        c.apply_env();
        assert_eq!(c.max_parallel_runs, before_pr);
        assert_eq!(c.capture_lines, before_cl);

        // Cleanup.
        std::env::remove_var("FORGE_ECHO_DEFAULT_MODEL");
        std::env::remove_var("FORGE_ECHO_MAX_PARALLEL_RUNS");
        std::env::remove_var("FORGE_ECHO_CAPTURE_LINES");
        std::env::remove_var("FORGE_ECHO_AUTONOMOUS_MAX_TOKENS_PER_DAY");
        std::env::remove_var("FORGE_ECHO_USER_MESSAGE_RATE_LIMIT_PER_MIN");
    }

    #[test]
    fn validate_and_fix_raises_invalid_to_defaults() {
        let mut c = EchoConfig {
            max_parallel_runs: 0,
            capture_lines: -10,
            default_model: String::new(),
            ..EchoConfig::default()
        };
        c.validate_and_fix();
        assert_eq!(c.max_parallel_runs, 1);
        assert_eq!(c.capture_lines, DEFAULT_CAPTURE_LINES);
        assert_eq!(c.default_model, DEFAULT_MODEL);
    }
}
