//! UserSettings — глобальные настройки уровня пользователя.
//!
//! Хранилище: один файл `~/.forge/user_settings.json`. В отличие от
//! `projects.json` / `themes.json` (per-data_dir) и `todos.json`
//! (per-project), эти настройки **не привязаны** ни к проекту, ни к
//! каталогу — они применяются ко всем сессиям конкретного пользователя
//! на машине.
//!
//! ### Состав настроек
//!
//! Все поля помечены `#[serde(default)]`, чтобы:
//!   1. Старый/пустой/неполный файл грузился без миграции.
//!   2. При полном отсутствии файла поведение системы было побитово
//!      идентично состоянию «до фичи user-settings» — критический
//!      инвариант, см. описание epic'а tw-z6l.
//!
//! Поля:
//!   - `todo_default_plan_mode` (bool, default false) — значение
//!     plan-mode-чекбокса по умолчанию при создании TODO.
//!   - `todo_default_priority` (u8, default 2, clamp 0..=4) — приоритет
//!     новой TODO по умолчанию.
//!   - `todo_default_issue_type` (String, default `"task"`) — тип
//!     issue по умолчанию для новой TODO.
//!   - `todo_plan_mode_suffix` (String, default `""`) — текст, который
//!     присоединяется к notify-сообщению при promote TODO с plan_mode=true.
//!     Пустая строка → используется константа `PLAN_MODE_SUFFIX` из `main.rs`.
//!   - `todo_confirm_delete` (bool, default true) — спрашивать ли
//!     подтверждение при удалении TODO (UI-флаг, backend-side просто хранит).
//!   - `todo_confirm_promote_on_drag` (bool, default false) — спрашивать
//!     ли подтверждение при promote через drag-and-drop.
//!
//! ### Persistence
//!
//! Atomic save: пишем в `<path>.tmp`, затем `fs::rename` поверх. На POSIX
//! rename атомарен в рамках одного mount-point — даже при `kill -9` в
//! момент записи получим либо старый, либо новый файл, но не битый.
//! Стратегия идентична [`crate::todos::save_locked`].
//!
//! ### Lazy file creation
//!
//! `UserSettingsStore::new(path)` НЕ создаёт файл, если его нет — просто
//! возвращает store с `UserSettings::default()`. Файл появится только при
//! первом успешном [`UserSettingsStore::patch`]. Это сохраняет
//! «нулевую конфигурацию» как состояние по умолчанию.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default-функция для `todo_default_priority` — medium (P2).
fn default_priority() -> u8 {
    2
}

/// Default-функция для `todo_default_issue_type` — `"task"`.
fn default_issue_type() -> String {
    "task".to_string()
}

/// Default-функция для `todo_confirm_delete` — true.
fn default_confirm_delete() -> bool {
    true
}

/// Максимально допустимое значение приоритета (4 = backlog).
const MAX_PRIORITY: u8 = 4;

/// Настройки уровня пользователя.
///
/// Сериализуется в `~/.forge/user_settings.json`. Все поля
/// `#[serde(default)]` — чтобы частичный / пустой файл считался
/// валидным состоянием (пропущенные поля берутся из [`Default`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSettings {
    #[serde(default)]
    pub todo_default_plan_mode: bool,
    #[serde(default = "default_priority")]
    pub todo_default_priority: u8,
    #[serde(default = "default_issue_type")]
    pub todo_default_issue_type: String,
    #[serde(default)]
    pub todo_plan_mode_suffix: String,
    #[serde(default = "default_confirm_delete")]
    pub todo_confirm_delete: bool,
    #[serde(default)]
    pub todo_confirm_promote_on_drag: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            todo_default_plan_mode: false,
            todo_default_priority: default_priority(),
            todo_default_issue_type: default_issue_type(),
            todo_plan_mode_suffix: String::new(),
            todo_confirm_delete: default_confirm_delete(),
            todo_confirm_promote_on_drag: false,
        }
    }
}

/// DTO для `PATCH /api/user-settings`. Все поля `Option<T>` — применяются
/// только `Some(..)`-варианты. Это позволяет клиенту менять одно поле, не
/// высылая полный объект.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchUserSettingsReq {
    #[serde(default)]
    pub todo_default_plan_mode: Option<bool>,
    #[serde(default)]
    pub todo_default_priority: Option<u8>,
    #[serde(default)]
    pub todo_default_issue_type: Option<String>,
    #[serde(default)]
    pub todo_plan_mode_suffix: Option<String>,
    #[serde(default)]
    pub todo_confirm_delete: Option<bool>,
    #[serde(default)]
    pub todo_confirm_promote_on_drag: Option<bool>,
}

#[derive(Debug)]
struct Inner {
    settings: UserSettings,
    path: PathBuf,
}

/// In-memory + on-disk хранилище [`UserSettings`].
///
/// Cheap-clonable: внутри `Arc<RwLock<Inner>>`. Один экземпляр на
/// процесс — кладётся в `AppState`. Все мутации проходят через `patch`,
/// который делает atomic save на диск.
#[derive(Debug, Clone)]
pub struct UserSettingsStore {
    inner: Arc<RwLock<Inner>>,
}

impl UserSettingsStore {
    /// Создаёт store. Если файл существует — пытается прочитать его и
    /// разобрать как [`UserSettings`]. При успехе использует разобранное
    /// значение; при ошибке парсинга — печатает warning в tracing и
    /// продолжает с [`UserSettings::default`] (политика «битый файл не
    /// блокирует работу»; пользователь поймёт по логам).
    ///
    /// Файл **не создаётся**, если его нет — это нужно для критического
    /// инварианта «нулевая конфигурация = поведение как до фичи».
    pub fn new(path: PathBuf) -> Self {
        let settings = match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<UserSettings>(&body) {
                Ok(s) => {
                    tracing::info!(
                        path = %path.display(),
                        "loaded user_settings.json"
                    );
                    s
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "failed to parse user_settings.json; falling back to defaults"
                    );
                    UserSettings::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %path.display(),
                    "user_settings.json not found; using defaults"
                );
                UserSettings::default()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = ?e,
                    "failed to read user_settings.json; falling back to defaults"
                );
                UserSettings::default()
            }
        };

        Self {
            inner: Arc::new(RwLock::new(Inner { settings, path })),
        }
    }

    /// Возвращает клон текущих настроек под read-lock'ом.
    pub fn get(&self) -> UserSettings {
        let inner = self.inner.read().expect("UserSettingsStore lock poisoned");
        inner.settings.clone()
    }

    /// Применяет частичный patch: только поля `Some(..)` обновляются.
    /// После мутации — atomic save на диск. Возвращает обновлённую копию.
    ///
    /// Валидация: `todo_default_priority` клампится в диапазон `0..=4`
    /// (значения > 4 приводятся к 4). Suffix принимается as-is (без trim) —
    /// клиент сам решает, какие пробелы хранить.
    pub fn patch(&self, payload: PatchUserSettingsReq) -> Result<UserSettings> {
        let mut inner = self.inner.write().expect("UserSettingsStore lock poisoned");
        if let Some(v) = payload.todo_default_plan_mode {
            inner.settings.todo_default_plan_mode = v;
        }
        if let Some(v) = payload.todo_default_priority {
            inner.settings.todo_default_priority = v.min(MAX_PRIORITY);
        }
        if let Some(v) = payload.todo_default_issue_type {
            inner.settings.todo_default_issue_type = v;
        }
        if let Some(v) = payload.todo_plan_mode_suffix {
            inner.settings.todo_plan_mode_suffix = v;
        }
        if let Some(v) = payload.todo_confirm_delete {
            inner.settings.todo_confirm_delete = v;
        }
        if let Some(v) = payload.todo_confirm_promote_on_drag {
            inner.settings.todo_confirm_promote_on_drag = v;
        }

        save_locked(&inner)?;
        Ok(inner.settings.clone())
    }
}

/// Атомарно сохраняет состояние под write-lock'ом.
///
/// Стратегия (как в `todos::save_locked` и `projects::ProjectStore::save`):
/// пишем в `<file>.tmp`, затем `rename` поверх. На POSIX rename атомарен
/// в рамках одного mount-point.
fn save_locked(inner: &Inner) -> Result<()> {
    let body = serde_json::to_vec_pretty(&inner.settings)
        .context("failed to serialize UserSettings")?;

    if let Some(parent) = inner.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir {}", parent.display())
            })?;
        }
    }

    let mut tmp = inner.path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &inner.path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp.display(),
            inner.path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уникальный путь во временной директории — UUID v4 как имя файла.
    fn tmp_path(label: &str) -> PathBuf {
        let id = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("devforge_user_settings_{label}_{id}.json"))
    }

    #[test]
    fn test_default_when_no_file() {
        let path = tmp_path("default_no_file");
        assert!(!path.exists(), "precondition: path must not exist");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert_eq!(s, UserSettings::default());
        // Файл НЕ должен появиться от чтения с дефолтами.
        assert!(
            !path.exists(),
            "file must not be created on read-only access"
        );
    }

    #[test]
    fn test_create_patch_reload() {
        let path = tmp_path("create_patch_reload");
        let store = UserSettingsStore::new(path.clone());
        assert_eq!(store.get(), UserSettings::default());

        let patched = store
            .patch(PatchUserSettingsReq {
                todo_default_plan_mode: Some(true),
                todo_default_priority: Some(3),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(patched.todo_default_plan_mode);
        assert_eq!(patched.todo_default_priority, 3);

        // Re-open: новый store на тот же путь должен видеть применённые изменения.
        let store2 = UserSettingsStore::new(path.clone());
        let s2 = store2.get();
        assert!(s2.todo_default_plan_mode);
        assert_eq!(s2.todo_default_priority, 3);
        // Остальные поля — дефолтные.
        assert_eq!(s2.todo_default_issue_type, "task");
        assert_eq!(s2.todo_plan_mode_suffix, "");
        assert!(s2.todo_confirm_delete);
        assert!(!s2.todo_confirm_promote_on_drag);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_priority_clamp() {
        let path = tmp_path("priority_clamp");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                todo_default_priority: Some(10),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.todo_default_priority, 4);

        // И на диске тоже 4 после reload.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().todo_default_priority, 4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_suffix_not_trimmed() {
        // Хранение suffix должно быть «как есть» — даже с ведущими пробелами.
        let path = tmp_path("suffix_no_trim");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                todo_plan_mode_suffix: Some("  spaced  ".to_string()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.todo_plan_mode_suffix, "  spaced  ");

        let _ = std::fs::remove_file(&path);
    }
}
