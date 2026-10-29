//! Конфигурация плагина `forge-tracker`.
//!
//! Фаза 1 знает ровно один параметр — путь к файлу хранилища. Это сделано
//! намеренно: конфигурация должна оставаться КОНСТРУКТОРОМ, а не глобальным
//! состоянием, иначе тесты начнут писать в живой
//! `~/.config/forge/tracker.json` пользователя.
//!
//! Два способа получить конфиг:
//!
//! * [`TrackerConfig::from_home`] — боевой путь `~/.config/forge/tracker.json`
//!   (как у `todos.json`, `vk.toml` и прочих файлов хоста);
//! * [`TrackerConfig::with_store_path`] — явный путь; ровно им пользуются
//!   тесты, подставляя каталог из `tempfile::tempdir()`.
//!
//! Плагин у трекера, в отличие от мессенджеров, не имеет секретов и токенов,
//! поэтому TOML-конфига здесь нет: включение делает флаг хоста (`--tracker`,
//! фаза 3), а всё остальное — данные внутри самого стора.

use std::path::{Path, PathBuf};

use crate::error::{Result, TrackerError};

/// Имя файла хранилища внутри `~/.config/forge/`.
pub const STORE_FILE_NAME: &str = "tracker.json";

/// Конфигурация трекера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerConfig {
    /// Абсолютный путь к JSON-файлу хранилища.
    store_path: PathBuf,
}

impl TrackerConfig {
    /// Конфиг с явным путём к файлу стора.
    ///
    /// Основной конструктор для тестов и нестандартных установок. Каталог
    /// создаётся не здесь, а при первой записи (см. `forge_atomic`).
    pub fn with_store_path(store_path: impl Into<PathBuf>) -> Self {
        Self {
            store_path: store_path.into(),
        }
    }

    /// Боевой конфиг: `$HOME/.config/forge/tracker.json`.
    ///
    /// # Errors
    ///
    /// Переменная `HOME` не задана.
    pub fn from_home() -> Result<Self> {
        Ok(Self::with_store_path(default_store_path()?))
    }

    /// Путь к файлу хранилища.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }
}

/// Путь к глобальному `~/.config/forge/tracker.json`.
///
/// Задачи трекера ГЛОБАЛЬНЫЕ: файл один на машину, группировки по корню
/// проекта в модели нет (в отличие от `tmux-web/src/todos.rs`).
///
/// # Errors
///
/// Переменная окружения `HOME` не задана.
pub fn default_store_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| TrackerError::Io("HOME env var is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("forge")
        .join(STORE_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_is_kept_as_is() {
        let cfg = TrackerConfig::with_store_path("/tmp/x/tracker.json");
        assert_eq!(cfg.store_path(), Path::new("/tmp/x/tracker.json"));
    }

    #[test]
    fn default_path_lands_in_config_forge() {
        let path = default_store_path().expect("HOME is set in test env");
        assert!(path.ends_with(".config/forge/tracker.json"), "{path:?}");
    }
}
