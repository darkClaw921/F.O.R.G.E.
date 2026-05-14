//! TODO-карточки проекта (Phase 1 — backend foundation).
//!
//! Хранит в памяти и на диске список TODO-карточек, сгруппированных по
//! `project_id`. Используется фронтендом как левая «storage» колонка
//! kanban-доски: задачи здесь — это идеи/наброски до момента «promote»
//! (превращения в полноценный bd-task с уведомлением в tmux).
//!
//! ### Хранилище
//!
//! Файл: `<project_root>/.forge/todos.json`. Каталог `.forge/` создаётся
//! при первом обращении (`TodoStore::new`). Запись — атомарная: пишем
//! в `<file>.tmp` + `rename`, чтобы при kill -9 во время save не оставить
//! битый JSON. Стратегия идентична `projects::ProjectStore::save`.
//!
//! ### Модель
//!
//! - [`Todo`] — карточка с `id` (UUID v4), `project_id`, `title`,
//!   `description`, `priority` (`u8`, 0..=4), `issue_type` (`String`),
//!   `labels` (`Vec<String>`), `created_at`, `updated_at` (RFC3339-строки
//!   UTC).
//! - [`TodoStore`] — `Arc<RwLock<Inner>>`-обёртка над
//!   `HashMap<project_id, Vec<Todo>>`, с lazy-load из todos.json и
//!   atomic save.
//!
//! ### Concurrency
//!
//! Все мутации проходят через `RwLock::write()`. Чтение — через
//! `read()`. Сохранение на диск происходит **внутри** write-lock'а, что
//! гарантирует: файл всегда отражает корректный snapshot. Для масштабов
//! «десятки/сотни TODO» это приемлемо (IO в нанах миллисекунд).
//!
//! ### Время
//!
//! Чтобы не тащить `chrono` ради двух полей, RFC3339-строки формируются
//! вручную из `SystemTime::now()` через алгоритм Howard Hinnant'а
//! (date.h: <https://howardhinnant.github.io/date_algorithms.html>).
//! Результат: `YYYY-MM-DDTHH:MM:SS.sssZ`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Одна TODO-карточка. Сериализуется в `todos.json` через serde.
///
/// Поля:
/// - `id` — UUID v4, уникален в рамках всего файла todos.json.
/// - `project_id` — id проекта (см. `projects::Project::id`).
/// - `title` — обязательное короткое название.
/// - `description` — опциональное подробное описание.
/// - `priority` — `u8`, 0..=4 (соответствует bd: 0=critical, 4=backlog).
///   По умолчанию `2` (medium).
/// - `issue_type` — строковый тип (task/feature/bug/...). Хранится как
///   `String`, чтобы не ограничивать алфавит — на момент промоушена
///   значение валидируется отдельно.
/// - `labels` — список произвольных меток.
/// - `created_at`, `updated_at` — RFC3339-строки в UTC, формируются
///   через [`now_rfc3339`].
///
/// Все опциональные/новые поля помечены `#[serde(default)]`, чтобы старые
/// файлы todos.json продолжали грузиться без миграции.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    pub id: String,
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default = "default_issue_type")]
    pub issue_type: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// План-мод: при promote_todo к notify-тексту добавляется суффикс
    /// "создай план для этой задачи" (точный текст — см. main.rs::PLAN_MODE_SUFFIX).
    /// Default false. `#[serde(default)]` обеспечивает совместимость со старыми todos.json.
    #[serde(default)]
    pub plan_mode: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Phase 3 — источник записи. Для локально-созданных TODO — всегда
    /// `"local"`. Сериализуется ВСЕГДА (даже при remote_mode=false), чтобы
    /// фронт получал унифицированный формат. `#[serde(default = "default_origin_local")]`
    /// делает поле опциональным при загрузке старых todos.json (где origin не было).
    #[serde(default = "default_origin_local")]
    pub origin: String,
}

/// Default-функция для `Todo::origin` и других DTO-полей origin.
/// Используется в `#[serde(default = "default_origin_local")]`.
pub fn default_origin_local() -> String {
    "local".to_string()
}

fn default_priority() -> u8 {
    2
}

fn default_issue_type() -> String {
    "task".to_string()
}

/// Файловый envelope для `todos.json`.
///
/// Хранится плоским списком, без группировки по проектам — это позволяет
/// будущим расширениям (фильтры, экспорт) пройтись по всем todo одним
/// циклом. Группировка по `project_id` происходит in-memory при загрузке.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TodosFile {
    #[serde(default)]
    todos: Vec<Todo>,
}

/// Внутреннее состояние хранилища (защищено `RwLock`).
#[derive(Debug, Default)]
struct Inner {
    /// project_id → список TODO в этом проекте.
    by_project: HashMap<String, Vec<Todo>>,
    /// Путь к `<project_root>/.forge/todos.json`.
    file_path: PathBuf,
}

/// Хранилище TODO-карточек, потокобезопасное и cheap-clonable
/// (внутри `Arc<RwLock<...>>`).
///
/// Использование:
/// ```ignore
/// let store = TodoStore::new(PathBuf::from("/path/to/project"))?;
/// let t = store.create("forge", "Refactor X", None, false)?;
/// let list = store.list("forge");
/// ```
#[derive(Debug, Clone)]
pub struct TodoStore {
    inner: Arc<RwLock<Inner>>,
}

impl TodoStore {
    /// Создаёт хранилище, привязанное к корню проекта.
    ///
    /// - Создаёт `<project_root>/.forge/`, если каталога нет.
    /// - Lazy-load `todos.json`: если файл отсутствует — старт с пустого
    ///   состояния, при первом `create` файл будет записан атомарно.
    /// - Если файл повреждён (невалидный JSON) — возвращает Err.
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let forge_dir = project_root.join(".forge");
        std::fs::create_dir_all(&forge_dir)
            .with_context(|| format!("failed to create {}", forge_dir.display()))?;
        let file_path = forge_dir.join("todos.json");

        let mut inner = Inner {
            by_project: HashMap::new(),
            file_path,
        };

        if inner.file_path.exists() {
            let raw = std::fs::read(&inner.file_path)
                .with_context(|| format!("failed to read {}", inner.file_path.display()))?;
            if !raw.is_empty() {
                let parsed: TodosFile = serde_json::from_slice(&raw)
                    .with_context(|| format!("failed to parse {}", inner.file_path.display()))?;
                for t in parsed.todos {
                    inner
                        .by_project
                        .entry(t.project_id.clone())
                        .or_default()
                        .push(t);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Возвращает все TODO в проекте. Порядок — insertion-order (то есть
    /// сначала старые, потом новые).
    pub fn list(&self, project_id: &str) -> Vec<Todo> {
        let inner = self.inner.read().expect("TodoStore lock poisoned");
        inner
            .by_project
            .get(project_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Возвращает TODO по `id`. Поиск идёт по всем проектам, потому что
    /// id уникален глобально (UUID v4).
    pub fn get(&self, id: &str) -> Option<Todo> {
        let inner = self.inner.read().expect("TodoStore lock poisoned");
        for list in inner.by_project.values() {
            if let Some(t) = list.iter().find(|t| t.id == id) {
                return Some(t.clone());
            }
        }
        None
    }

    /// Создаёт новый TODO с дефолтами `priority=2`, `issue_type="task"`,
    /// пустыми `labels`. Генерирует UUID v4 и timestamp `now`.
    /// После мутации — atomic save.
    pub fn create(
        &self,
        project_id: &str,
        title: &str,
        description: Option<String>,
        plan_mode: bool,
    ) -> Result<Todo> {
        let now = now_rfc3339();
        let todo = Todo {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            title: title.to_string(),
            description,
            priority: default_priority(),
            issue_type: default_issue_type(),
            labels: Vec::new(),
            plan_mode,
            created_at: now.clone(),
            updated_at: now,
            origin: default_origin_local(),
        };
        let mut inner = self.inner.write().expect("TodoStore lock poisoned");
        inner
            .by_project
            .entry(project_id.to_string())
            .or_default()
            .push(todo.clone());
        save_locked(&inner)?;
        Ok(todo)
    }

    /// Обновляет `title` и/или `description` у существующей карточки.
    ///
    /// Семантика параметров:
    /// - `title: None` — не трогать.
    /// - `description: None` — не трогать.
    /// - `description: Some(None)` — установить в `None` (очистить).
    /// - `description: Some(Some(s))` — записать строку.
    ///
    /// Возвращает обновлённую копию или `None`, если id не найден.
    /// При успехе обновляет `updated_at` и сохраняет файл.
    pub fn update(
        &self,
        id: &str,
        title: Option<String>,
        description: Option<Option<String>>,
        plan_mode: Option<bool>,
    ) -> Result<Option<Todo>> {
        let mut inner = self.inner.write().expect("TodoStore lock poisoned");
        let mut found: Option<Todo> = None;
        for list in inner.by_project.values_mut() {
            if let Some(t) = list.iter_mut().find(|t| t.id == id) {
                if let Some(new_title) = title {
                    t.title = new_title;
                }
                if let Some(new_desc) = description {
                    t.description = new_desc;
                }
                if let Some(pm) = plan_mode {
                    t.plan_mode = pm;
                }
                t.updated_at = now_rfc3339();
                found = Some(t.clone());
                break;
            }
        }
        if found.is_some() {
            save_locked(&inner)?;
        }
        Ok(found)
    }

    /// Удаляет TODO по `id`. Возвращает `true`, если удалили.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("TodoStore lock poisoned");
        let mut removed = false;
        for list in inner.by_project.values_mut() {
            let before = list.len();
            list.retain(|t| t.id != id);
            if list.len() != before {
                removed = true;
                break;
            }
        }
        if removed {
            save_locked(&inner)?;
        }
        Ok(removed)
    }

    /// Принудительно сохраняет текущее состояние на диск.
    /// Используется в тестах и при экстренном flush.
    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        let inner = self.inner.read().expect("TodoStore lock poisoned");
        save_locked(&inner)
    }
}

/// Атомарно сохраняет состояние под (write|read)-lock'ом.
///
/// Стратегия (как в `projects::ProjectStore::save`): пишем в
/// `<file>.tmp`, затем `rename` поверх. На POSIX rename атомарен в
/// рамках одного mount-point — даже при kill -9 в момент записи
/// получим либо старый, либо новый файл, но не битый.
fn save_locked(inner: &Inner) -> Result<()> {
    let mut all: Vec<Todo> = Vec::new();
    for list in inner.by_project.values() {
        all.extend(list.iter().cloned());
    }
    let envelope = TodosFile { todos: all };
    let body =
        serde_json::to_vec_pretty(&envelope).context("failed to serialize TodosFile")?;

    let mut tmp = inner.file_path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &inner.file_path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp.display(),
            inner.file_path.display()
        )
    })?;
    Ok(())
}

/// Возвращает текущее UTC-время в RFC3339-строке без подключения chrono.
///
/// Формат: `YYYY-MM-DDTHH:MM:SS.sssZ`. Простая конверсия Unix-секунд
/// через algoritm от Howard Hinnant (date.h).
pub fn now_rfc3339() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    format_unix_utc(secs, millis)
}

/// Конвертирует Unix-таймстамп в RFC3339-строку UTC.
/// См. <https://howardhinnant.github.io/date_algorithms.html>.
fn format_unix_utc(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let hour = (time_of_day / 3600) as u32;
    let minute = ((time_of_day % 3600) / 60) as u32;
    let second = (time_of_day % 60) as u32;

    // days_from_civil(y, m, d) inverse:
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, m, d, hour, minute, second, millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-todos-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn creates_forge_dir_and_loads_empty() {
        let dir = tempdir("init");
        let store = TodoStore::new(dir.clone()).unwrap();
        assert!(dir.join(".forge").is_dir());
        assert!(store.list("any").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_get_update_delete_roundtrip() {
        let dir = tempdir("crud");
        let store = TodoStore::new(dir.clone()).unwrap();

        let t = store
            .create("forge", "First task", Some("with desc".into()), false)
            .unwrap();
        assert_eq!(t.title, "First task");
        assert_eq!(t.priority, 2);
        assert_eq!(t.issue_type, "task");
        assert_eq!(t.id.len(), 36);
        assert!(!t.plan_mode);
        assert!(t.created_at.ends_with('Z'));

        let fetched = store.get(&t.id).unwrap();
        assert_eq!(fetched.id, t.id);

        let updated = store
            .update(&t.id, Some("Renamed".into()), Some(None), Some(true))
            .unwrap()
            .unwrap();
        assert_eq!(updated.title, "Renamed");
        assert!(updated.description.is_none());
        assert!(updated.plan_mode);
        assert!(updated.updated_at >= t.updated_at);

        assert!(store.delete(&t.id).unwrap());
        assert!(store.get(&t.id).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tempdir("persist");
        {
            let store = TodoStore::new(dir.clone()).unwrap();
            store.create("forge", "Persisted", None, false).unwrap();
        }
        let store2 = TodoStore::new(dir.clone()).unwrap();
        let list = store2.list("forge");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Persisted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_isolation_per_project() {
        let dir = tempdir("isolate");
        let store = TodoStore::new(dir.clone()).unwrap();
        store.create("a", "in-a", None, false).unwrap();
        store.create("b", "in-b1", None, false).unwrap();
        store.create("b", "in-b2", None, false).unwrap();

        assert_eq!(store.list("a").len(), 1);
        assert_eq!(store.list("b").len(), 2);
        assert_eq!(store.list("missing").len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rfc3339_format_basic() {
        // 2024-01-01T00:00:00Z = 1_704_067_200
        assert_eq!(
            format_unix_utc(1_704_067_200, 0),
            "2024-01-01T00:00:00.000Z"
        );
        // 1970-01-01T00:00:00Z
        assert_eq!(format_unix_utc(0, 0), "1970-01-01T00:00:00.000Z");
        // Provide a known-good timestamp: 2026-05-10T12:34:56Z
        // Compute: from 1970-01-01 to 2026-05-10:
        //   56 full years: each common 365d, leap +1d.
        //   We trust Hinnant's algorithm; just assert structural correctness.
        let s = format_unix_utc(1_778_416_496, 789);
        assert!(s.starts_with("2026-05-"));
        assert!(s.ends_with(".789Z"));
    }

    #[test]
    fn corrupt_file_yields_err() {
        let dir = tempdir("corrupt");
        let forge_dir = dir.join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(forge_dir.join("todos.json"), b"not json").unwrap();
        let res = TodoStore::new(dir.clone());
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
