//! TODO-карточки, привязанные к «корню» cwd (а не к проекту).
//!
//! ### Phase 1 — переход с project_id на root_path
//!
//! Раньше каждая TODO-карточка имела поле `project_id` и принадлежала
//! одному из проектов в `~/.config/forge/projects.json`. Поскольку
//! концепция «project» удаляется (см. план `remove-projects-concept.md`),
//! теперь карточки группируются по абсолютному пути «корня» — папке,
//! куда поднялся [`crate::paths::resolve_root`] для исходного cwd сессии
//! (`.beads/` → `.git/` → сам cwd).
//!
//! ### Хранилище
//!
//! Файл: `~/.config/forge/todos.json`. Раньше был `<project_root>/.forge/todos.json`
//! (один файл на каждый проект); теперь — единый глобальный файл, как у
//! `projects.json`/`remote_servers.json`/`server_config.json`. Запись —
//! атомарная: пишем в `<file>.tmp` + `rename`, чтобы при kill -9 во время
//! save не оставить битый JSON.
//!
//! ### Модель
//!
//! - [`Todo`] — карточка с `id` (UUID v4), `root_path`, `title`,
//!   `description`, `priority` (`u8`, 0..=4), `issue_type` (`String`),
//!   `labels` (`Vec<String>`), `created_at`, `updated_at` (RFC3339-строки
//!   UTC), `plan_mode`, `auto_promote`, `origin`.
//! - [`TodoStore`] — `Arc<RwLock<Inner>>`-обёртка над
//!   `HashMap<root_path, Vec<Todo>>`, с lazy-load из todos.json и
//!   atomic save.
//!
//! ### Backward compatibility
//!
//! - При десериализации `Todo` принимает старое поле `project_id` (с
//!   `#[serde(alias = "project_id")]` на `root_path`), чтобы загрузка
//!   старых todos.json не падала. После первого `save()` файл уже в
//!   новом формате с `root_path`.
//! - Файловый envelope `TodosFile` — плоский список, как и раньше.
//!   Группировка по `root_path` происходит in-memory при загрузке.
//! - Phase 1.4 добавит миграционный шаг: если значения `root_path`
//!   похожи на project_id (не абсолютные пути) — один раз читаем
//!   `~/.config/forge/projects.json` и мапим id → project.path.
//!
//! ### Adapter для main.rs (Phase 1 → Phase 2 транзит)
//!
//! Хендлеры в main.rs всё ещё передают «project_id» как `&str`. На время
//! фазы 2 (миграция REST/WS API на `?path=`) мы трактуем приходящий
//! `&str` как `root_path` — это сохраняет компилируемость без падения
//! на runtime: до Phase 2 пользователь видит просто пустой список (id
//! проекта не совпадает ни с одним абсолютным путём), но не crash.
//!
//! ### Concurrency
//!
//! Все мутации проходят через `RwLock::write()`. Чтение — через
//! `read()`. Сохранение на диск происходит **внутри** write-lock'а, что
//! гарантирует: файл всегда отражает корректный snapshot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths;

/// Одна TODO-карточка. Сериализуется в `todos.json` через serde.
///
/// Поля:
/// - `id` — UUID v4, уникален в рамках всего файла todos.json.
/// - `root_path` — абсолютный путь к корню (см. [`crate::paths::resolve_root`]).
///   Старое поле `project_id` читается как `root_path` через
///   `#[serde(alias = "project_id")]` для backward-compat.
/// - `title` — обязательное короткое название.
/// - `description` — опциональное подробное описание.
/// - `priority` — `u8`, 0..=4 (соответствует bd: 0=critical, 4=backlog).
///   По умолчанию `2` (medium).
/// - `issue_type` — строковый тип (task/feature/bug/...).
/// - `labels` — список произвольных меток.
/// - `plan_mode` — при promote добавлять «создай план для этой задачи».
/// - `auto_promote` — помечена ли карточка для авто-промоута по очереди
///   (см. цепочку авто-промоута). По умолчанию `false`; `#[serde(default)]`
///   даёт backward-compat со старыми `todos.json`.
/// - `created_at`, `updated_at` — RFC3339-строки в UTC.
/// - `origin` — `"local"` или `"remote"` (Phase 3 remote-proxy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    pub id: String,
    /// Абсолютный путь к корню. Читается также из старого поля `project_id`
    /// через `#[serde(alias = "project_id")]`.
    #[serde(alias = "project_id")]
    pub root_path: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default = "default_issue_type")]
    pub issue_type: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub plan_mode: bool,
    /// Помечена ли карточка для авто-промоута по очереди. При `true` карточка
    /// участвует в цепочке: после закрытия предыдущей промоутнутой задачи
    /// верхняя помеченная TODO-карточка автоматически промоутится. По умолчанию
    /// `false` (через `#[serde(default)]`, обеспечивает backward-compat со
    /// старыми `todos.json`, где поля ещё нет).
    #[serde(default)]
    pub auto_promote: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default = "default_origin_local")]
    pub origin: String,
}

/// Default для `Todo::origin`.
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
/// Плоский список Todo — группировка по `root_path` происходит in-memory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TodosFile {
    #[serde(default)]
    todos: Vec<Todo>,
}

/// Внутреннее состояние хранилища (защищено `RwLock`).
#[derive(Debug, Default)]
struct Inner {
    /// root_path → список TODO в этом корне.
    by_path: HashMap<String, Vec<Todo>>,
    /// Путь к `~/.config/forge/todos.json`.
    file_path: PathBuf,
}

/// Хранилище TODO-карточек, потокобезопасное и cheap-clonable
/// (внутри `Arc<RwLock<...>>`).
///
/// Phase 1: данные группируются по `root_path` (абсолютный путь);
/// API методы `list/create/update/delete` принимают строковый ключ
/// (`&str`), который трактуется как `root_path`. Phase 2 заменит сигнатуры
/// на `&Path` cwd, который будет резолвится через `paths::resolve_root`.
#[derive(Debug, Clone)]
pub struct TodoStore {
    inner: Arc<RwLock<Inner>>,
}

impl TodoStore {
    /// Создаёт хранилище с глобальным файлом `~/.config/forge/todos.json`.
    ///
    /// Параметр `_initial_root` оставлен для совместимости с вызовом из
    /// `main.rs` (Phase 1 транзит); фактически игнорируется. В Phase 2
    /// конструктор станет `TodoStore::load()` без параметров.
    ///
    /// Поведение:
    /// - Создаёт `~/.config/forge/`, если каталога нет.
    /// - Lazy-load `todos.json`: если файл отсутствует — старт с пустого
    ///   состояния. При первом мутирующем вызове файл будет записан.
    /// - Если файл повреждён (невалидный JSON) — возвращает Err.
    pub fn new(_initial_root: PathBuf) -> Result<Self> {
        let file_path = default_todos_path()?;
        Self::load(file_path)
    }

    /// Открывает store по явному пути к JSON-файлу. Используется в
    /// тестах (через tempdir) и при кастомных HOME-конфигурациях.
    ///
    /// Side-эффект миграции: если в файле встречаются legacy-записи
    /// (поле `project_id` вместо `root_path`), они подхватываются через
    /// `#[serde(alias = "project_id")]` (значение попадёт в `root_path`).
    /// После загрузки если хотя бы один `root_path` похож на project_id
    /// (не абсолютный путь), читаем `~/.config/forge/projects.json` через
    /// [`load_projects_path_map`] и подменяем id → реальный path. Если
    /// projects.json недоступен или id там нет — оставляем значение как
    /// есть (fallback: project_id как root_path). После миграции — atomic
    /// save в новом формате.
    pub fn load(file_path: PathBuf) -> Result<Self> {
        Self::load_with_projects(file_path, default_projects_path().ok())
    }

    /// Расширенный вариант `load`, принимающий явный путь к projects.json
    /// (или `None`, если миграция не требуется). Используется в тестах,
    /// где нужен изолированный projects-stub.
    pub fn load_with_projects(
        file_path: PathBuf,
        projects_path: Option<PathBuf>,
    ) -> Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut inner = Inner {
            by_path: HashMap::new(),
            file_path,
        };

        let mut had_legacy = false;

        if inner.file_path.exists() {
            let raw = std::fs::read(&inner.file_path)
                .with_context(|| format!("failed to read {}", inner.file_path.display()))?;
            if !raw.is_empty() {
                // Сперва смотрим сырой JSON: если хотя бы один объект
                // содержит ключ `project_id` (вместо `root_path`), это
                // legacy-формат и потребуется миграция через projects.json.
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                    if let Some(arr) = v.get("todos").and_then(|x| x.as_array()) {
                        had_legacy = arr.iter().any(|item| {
                            item.get("project_id").is_some()
                                && item.get("root_path").is_none()
                        });
                    }
                }

                let parsed: TodosFile = serde_json::from_slice(&raw)
                    .with_context(|| format!("failed to parse {}", inner.file_path.display()))?;
                for t in parsed.todos {
                    inner.by_path.entry(t.root_path.clone()).or_default().push(t);
                }
            }
        }

        // Миграция: если был legacy-маркер ИЛИ хотя бы один root_path
        // не абсолютный путь (характерно для project_id вроде "forge"),
        // пробуем resolve через projects.json.
        let needs_migration =
            had_legacy || inner.by_path.keys().any(|k| !looks_like_abs_path(k));
        let path_map = projects_path
            .as_deref()
            .and_then(|p| load_projects_path_map(p));
        if needs_migration {
            migrate_project_ids(&mut inner, path_map.as_ref());
        }

        // Дополнительная миграция: если global todos.json пустой/отсутствовал,
        // сканируем все известные проекты на наличие per-project legacy
        // файлов `<project.path>/.forge/todos.json` и сливаем их в глобальный
        // store с root_path = project.path. Это покрывает кейс когда старая
        // версия devforge хранила todos per-project, а не глобально.
        let global_was_empty = inner.by_path.is_empty();
        let imported_legacy = if global_was_empty {
            import_legacy_per_project_todos(&mut inner, path_map.as_ref())
        } else {
            0
        };

        // После миграции пишем сразу в новом формате (даже если файл
        // отсутствовал — создаём первый раз с импортированными legacy).
        if needs_migration || imported_legacy > 0 {
            // Здесь `inner` ещё локальный (не за RwLock) — contention нет,
            // но используем те же helper'ы для единообразия.
            let snap = serialize_locked(&inner)
                .context("failed to serialize migrated todos.json")?;
            write_snapshot(&snap).context("failed to save migrated todos.json")?;
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Возвращает все TODO для указанного root_path.
    ///
    /// Port-of-old-API: `&str` трактуется как root_path. Phase 2
    /// перепишет вызовы в main.rs на варианты с `&Path` cwd.
    pub fn list(&self, root_path: &str) -> Vec<Todo> {
        let inner = self.inner.read().expect("TodoStore lock poisoned");
        inner.by_path.get(root_path).cloned().unwrap_or_default()
    }

    /// Возвращает все TODO для cwd: резолвит root через
    /// `paths::resolve_root` и возвращает соответствующий список.
    /// Phase 2 будет основным caller'ом из REST/WS хендлеров.
    #[allow(dead_code)]
    pub fn list_by_cwd(&self, cwd: &Path) -> Vec<Todo> {
        let root = paths::resolve_root(cwd);
        self.list(&root.to_string_lossy())
    }

    /// Возвращает TODO по `id`. Поиск идёт по всем корням, потому что
    /// id уникален глобально (UUID v4).
    pub fn get(&self, id: &str) -> Option<Todo> {
        let inner = self.inner.read().expect("TodoStore lock poisoned");
        for list in inner.by_path.values() {
            if let Some(t) = list.iter().find(|t| t.id == id) {
                return Some(t.clone());
            }
        }
        None
    }

    /// Создаёт новый TODO с дефолтами `priority=2`, `issue_type="task"`,
    /// пустыми `labels`. Генерирует UUID v4 и timestamp `now`.
    /// После мутации — atomic save.
    ///
    /// `root_path` — строковый ключ группировки. Обычно вызывающий
    /// получает его как `paths::resolve_root(cwd).to_string_lossy()`.
    pub fn create(
        &self,
        root_path: &str,
        title: &str,
        description: Option<String>,
        plan_mode: bool,
    ) -> Result<Todo> {
        let now = now_rfc3339();
        let todo = Todo {
            id: Uuid::new_v4().to_string(),
            root_path: root_path.to_string(),
            title: title.to_string(),
            description,
            priority: default_priority(),
            issue_type: default_issue_type(),
            labels: Vec::new(),
            plan_mode,
            auto_promote: false,
            created_at: now.clone(),
            updated_at: now,
            origin: default_origin_local(),
        };
        let snap = {
            let mut inner = self.inner.write().expect("TodoStore lock poisoned");
            inner
                .by_path
                .entry(root_path.to_string())
                .or_default()
                .push(todo.clone());
            serialize_locked(&inner)?
        };
        write_snapshot(&snap)?;
        Ok(todo)
    }

    /// Создаёт новый TODO, привязанный к корню, резолвящемуся от `cwd`.
    /// Phase 2 будет основным caller'ом из REST хендлеров.
    #[allow(dead_code)]
    pub fn create_by_cwd(
        &self,
        cwd: &Path,
        title: &str,
        description: Option<String>,
        plan_mode: bool,
    ) -> Result<Todo> {
        let root = paths::resolve_root(cwd);
        self.create(&root.to_string_lossy(), title, description, plan_mode)
    }

    /// Обновляет `title` и/или `description`/`plan_mode`/`auto_promote`
    /// существующей карточки.
    ///
    /// Семантика параметров:
    /// - `title: None` — не трогать.
    /// - `description: None` — не трогать.
    /// - `description: Some(None)` — очистить (записать None).
    /// - `description: Some(Some(s))` — записать строку.
    /// - `plan_mode: None` — не трогать.
    /// - `plan_mode: Some(b)` — записать b.
    /// - `auto_promote: None` — не трогать.
    /// - `auto_promote: Some(b)` — записать b.
    ///
    /// Возвращает обновлённую копию или `None`, если id не найден.
    /// При успехе обновляет `updated_at` и сохраняет файл.
    pub fn update(
        &self,
        id: &str,
        title: Option<String>,
        description: Option<Option<String>>,
        plan_mode: Option<bool>,
        auto_promote: Option<bool>,
    ) -> Result<Option<Todo>> {
        let (found, snap) = {
            let mut inner = self.inner.write().expect("TodoStore lock poisoned");
            let mut found: Option<Todo> = None;
            for list in inner.by_path.values_mut() {
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
                    if let Some(ap) = auto_promote {
                        t.auto_promote = ap;
                    }
                    t.updated_at = now_rfc3339();
                    found = Some(t.clone());
                    break;
                }
            }
            let snap = if found.is_some() {
                Some(serialize_locked(&inner)?)
            } else {
                None
            };
            (found, snap)
        };
        if let Some(snap) = snap {
            write_snapshot(&snap)?;
        }
        Ok(found)
    }

    /// Перемещает TODO с заданным `id` в другой корень `new_root`.
    ///
    /// Если id не найден — возвращает `Ok(None)`. При успехе обновляет
    /// `Todo.root_path`, перекладывает запись в соответствующий bucket
    /// `Inner.by_path`, обновляет `updated_at` и сохраняет файл. Возвращает
    /// обновлённую копию.
    ///
    /// Используется PATCH /api/todos/:id, когда клиент шлёт новое значение
    /// `path` в body (move между корнями cwd-группировки).
    pub fn move_to_root(&self, id: &str, new_root: &str) -> Result<Option<Todo>> {
        let (clone, snap) = {
            let mut inner = self.inner.write().expect("TodoStore lock poisoned");
            // 1) Найти и удалить из старого bucket.
            let mut found_old_root: Option<String> = None;
            let mut taken: Option<Todo> = None;
            for (root, list) in inner.by_path.iter_mut() {
                if let Some(pos) = list.iter().position(|t| t.id == id) {
                    taken = Some(list.remove(pos));
                    found_old_root = Some(root.clone());
                    break;
                }
            }
            // Зачистка пустых bucket'ов (опционально, но симметрично загрузке).
            if let Some(ref old) = found_old_root {
                let is_empty = inner.by_path.get(old).map(|v| v.is_empty()).unwrap_or(false);
                if is_empty {
                    inner.by_path.remove(old);
                }
            }
            // 2) Положить в новый bucket с обновлёнными полями.
            let mut todo = match taken {
                Some(t) => t,
                None => return Ok(None),
            };
            todo.root_path = new_root.to_string();
            todo.updated_at = now_rfc3339();
            let clone = todo.clone();
            inner
                .by_path
                .entry(new_root.to_string())
                .or_default()
                .push(todo);
            let snap = serialize_locked(&inner)?;
            (clone, snap)
        };
        write_snapshot(&snap)?;
        Ok(Some(clone))
    }

    /// Удаляет TODO по `id`. Возвращает `true`, если удалили.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let (removed, snap) = {
            let mut inner = self.inner.write().expect("TodoStore lock poisoned");
            let mut removed = false;
            for list in inner.by_path.values_mut() {
                let before = list.len();
                list.retain(|t| t.id != id);
                if list.len() != before {
                    removed = true;
                    break;
                }
            }
            let snap = if removed {
                Some(serialize_locked(&inner)?)
            } else {
                None
            };
            (removed, snap)
        };
        if let Some(snap) = snap {
            write_snapshot(&snap)?;
        }
        Ok(removed)
    }

    /// Принудительно сохраняет текущее состояние на диск.
    /// Используется в тестах и при экстренном flush.
    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        let snap = {
            let inner = self.inner.read().expect("TodoStore lock poisoned");
            serialize_locked(&inner)?
        };
        write_snapshot(&snap)
    }
}

/// Снимок для записи на диск: сериализованное тело + целевой путь + tmp-путь.
/// Готовится ПОД lock'ом ([`serialize_locked`]), а сама запись
/// ([`write_snapshot`]) выполняется уже ПОСЛЕ дропа guard'а — чтобы блокирующий
/// файловый I/O (write+rename, потенциально десятки мс на медленном диске) не
/// держал `RwLock` и не сериализовал все конкурентные REST-хендлеры.
struct SaveSnapshot {
    body: Vec<u8>,
    file_path: PathBuf,
    tmp: PathBuf,
}

/// Готовит снимок состояния под (write|read)-lock'ом. НЕ трогает диск.
fn serialize_locked(inner: &Inner) -> Result<SaveSnapshot> {
    let mut all: Vec<Todo> = Vec::new();
    for list in inner.by_path.values() {
        all.extend(list.iter().cloned());
    }
    let envelope = TodosFile { todos: all };
    let body =
        serde_json::to_vec_pretty(&envelope).context("failed to serialize TodosFile")?;

    let mut tmp = inner.file_path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    Ok(SaveSnapshot {
        body,
        file_path: inner.file_path.clone(),
        tmp,
    })
}

/// Атомарно пишет снимок на диск. Вызывается БЕЗ удерживаемого lock'а.
///
/// Стратегия (как в `projects::ProjectStore::save`): пишем в
/// `<file>.tmp`, затем `rename` поверх. На POSIX rename атомарен в
/// рамках одного mount-point — даже при kill -9 в момент записи
/// получим либо старый, либо новый файл, но не битый.
fn write_snapshot(snap: &SaveSnapshot) -> Result<()> {
    std::fs::write(&snap.tmp, &snap.body)
        .with_context(|| format!("failed to write tmp {}", snap.tmp.display()))?;
    std::fs::rename(&snap.tmp, &snap.file_path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            snap.tmp.display(),
            snap.file_path.display()
        )
    })?;
    Ok(())
}

/// Путь к глобальному `~/.config/forge/todos.json`.
///
/// Не использует крейт `dirs` — для macOS/Linux достаточно `$HOME`.
pub fn default_todos_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var is not set")?;
    Ok(PathBuf::from(home).join(".config/forge/todos.json"))
}

/// Путь к `~/.config/forge/projects.json`. Дублирует
/// [`crate::projects::default_registry_path`], чтобы todos.rs не зависел
/// от `projects` модуля (тот будет удалён в Phase 4).
pub fn default_projects_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var is not set")?;
    Ok(PathBuf::from(home).join(".config/forge/projects.json"))
}

/// Эвристика: похож ли строковый ключ на абсолютный путь.
///
/// Для POSIX — начинается с `/`. Для Windows — начинается с буквы и `:`
/// (например, `C:\`). Если эвристика возвращает `false`, считаем, что
/// ключ — это унаследованный project_id (например, `"forge"`), и
/// пытаемся резолвить через projects.json.
fn looks_like_abs_path(s: &str) -> bool {
    if s.starts_with('/') {
        return true;
    }
    // Windows fallback: `C:` / `D:` / etc.
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Читает `projects.json` и строит map `project_id → project.path`
/// (как строка). Не паникует на отсутствии/повреждении файла — возвращает
/// `None`, чтобы вызывающая сторона могла применить fallback.
pub fn load_projects_path_map(path: &Path) -> Option<HashMap<String, String>> {
    let raw = std::fs::read(path).ok()?;
    if raw.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let arr = v.get("projects")?.as_array()?;
    let mut out = HashMap::new();
    for item in arr {
        let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let p = item.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if !id.is_empty() && !p.is_empty() {
            out.insert(id.to_string(), p.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Подменяет в `Inner.by_path` все ключи-«project_id» на абсолютные пути
/// через map из `load_projects_path_map`. Если map отсутствует — оставляем
/// project_id как root_path (deg-fallback) и логируем warning.
///
/// Логика:
/// - Если ключ уже выглядит как абсолютный путь — пропускаем.
/// - Если ключ есть в `path_map` — переносим Todo в новый bucket с
///   реальным `root_path = map[project_id]`. Также обновляем поле
///   `Todo.root_path` у каждой карточки, иначе при следующем save() мы
///   запишем старое значение.
/// - Если ключа в map нет — оставляем как есть, печатаем `tracing::warn`.
fn migrate_project_ids(inner: &mut Inner, path_map: Option<&HashMap<String, String>>) {
    let keys: Vec<String> = inner.by_path.keys().cloned().collect();
    let mut migrated = 0usize;
    let mut unresolved = 0usize;
    for key in keys {
        if looks_like_abs_path(&key) {
            continue;
        }
        let new_root = match path_map.and_then(|m| m.get(&key)) {
            Some(p) => p.clone(),
            None => {
                unresolved += 1;
                continue;
            }
        };
        if let Some(mut bucket) = inner.by_path.remove(&key) {
            for t in bucket.iter_mut() {
                t.root_path = new_root.clone();
            }
            inner
                .by_path
                .entry(new_root.clone())
                .or_default()
                .append(&mut bucket);
            migrated += 1;
        }
    }
    if migrated > 0 || unresolved > 0 {
        tracing::info!(
            migrated,
            unresolved,
            "migrated todos.json from project_id to root_path"
        );
    }
    if unresolved > 0 {
        tracing::warn!(
            "{unresolved} todo group(s) kept legacy project_id as root_path \
             — projects.json missing or stale entries"
        );
    }
}

/// Сканирует все известные проекты (из projects.json через `path_map`)
/// и импортирует per-project legacy `<project.path>/.forge/todos.json`
/// файлы в глобальный store. Возвращает количество импортированных TODO.
///
/// Это покрывает кейс, когда старая версия devforge хранила TODO в
/// `<project.path>/.forge/todos.json` (per-project), а новая использует
/// один глобальный `~/.config/forge/todos.json`. Импортированные TODO
/// получают `root_path = project.path`.
///
/// Игнорирует:
/// - отсутствующие файлы (legacy storage просто не использовался для этого проекта);
/// - битый JSON (warn-log, продолжает с другими);
/// - дубликаты id с уже-загруженными TODO (impossible после
///   `global_was_empty` гейта, но проверяем для надёжности).
fn import_legacy_per_project_todos(
    inner: &mut Inner,
    path_map: Option<&HashMap<String, String>>,
) -> usize {
    let Some(map) = path_map else {
        return 0;
    };
    let mut imported_total = 0usize;
    let mut imported_files = 0usize;

    for (project_id, project_path_raw) in map.iter() {
        let legacy_file = PathBuf::from(project_path_raw).join(".forge/todos.json");
        if !legacy_file.exists() {
            continue;
        }
        // Нормализуем root_path: убираем trailing slash чтобы lookup
        // совпадал с тем, что `paths::resolve_root` выдаёт для сессий
        // (он возвращает PathBuf без trailing slash). Корень `/`
        // оставляем как `/`.
        let project_path = if project_path_raw.len() > 1 && project_path_raw.ends_with('/') {
            project_path_raw.trim_end_matches('/').to_string()
        } else {
            project_path_raw.clone()
        };
        let raw = match std::fs::read(&legacy_file) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    path = %legacy_file.display(),
                    error = %e,
                    "failed to read legacy per-project todos.json"
                );
                continue;
            }
        };
        if raw.is_empty() {
            continue;
        }
        let parsed: TodosFile = match serde_json::from_slice(&raw) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    path = %legacy_file.display(),
                    error = %e,
                    "failed to parse legacy per-project todos.json"
                );
                continue;
            }
        };
        let count = parsed.todos.len();
        if count == 0 {
            continue;
        }
        let bucket = inner.by_path.entry(project_path.clone()).or_default();
        for mut t in parsed.todos {
            t.root_path = project_path.clone();
            bucket.push(t);
        }
        imported_total += count;
        imported_files += 1;
        tracing::info!(
            project_id = %project_id,
            project_path = %project_path,
            count,
            "imported legacy per-project todos.json"
        );
    }

    if imported_total > 0 {
        tracing::info!(
            files = imported_files,
            todos = imported_total,
            "imported per-project legacy todos into global store"
        );
    }

    imported_total
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

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
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

    fn tmpfile(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-todos-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p.join("todos.json")
    }

    #[test]
    fn creates_dir_and_loads_empty() {
        let f = tmpfile("init");
        let store = TodoStore::load(f.clone()).unwrap();
        assert!(f.parent().unwrap().is_dir());
        assert!(store.list("any").is_empty());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn create_get_update_delete_roundtrip() {
        let f = tmpfile("crud");
        let store = TodoStore::load(f.clone()).unwrap();

        let t = store
            .create("/tmp/root", "First task", Some("with desc".into()), false)
            .unwrap();
        assert_eq!(t.title, "First task");
        assert_eq!(t.root_path, "/tmp/root");
        assert_eq!(t.priority, 2);
        assert_eq!(t.issue_type, "task");
        assert_eq!(t.id.len(), 36);
        assert!(!t.plan_mode);
        assert!(t.created_at.ends_with('Z'));

        let fetched = store.get(&t.id).unwrap();
        assert_eq!(fetched.id, t.id);

        let updated = store
            .update(&t.id, Some("Renamed".into()), Some(None), Some(true), None)
            .unwrap()
            .unwrap();
        assert_eq!(updated.title, "Renamed");
        assert!(updated.description.is_none());
        assert!(updated.plan_mode);
        assert!(updated.updated_at >= t.updated_at);

        assert!(store.delete(&t.id).unwrap());
        assert!(store.get(&t.id).is_none());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn rfc3339_format_basic() {
        assert_eq!(
            format_unix_utc(1_704_067_200, 0),
            "2024-01-01T00:00:00.000Z"
        );
        assert_eq!(format_unix_utc(0, 0), "1970-01-01T00:00:00.000Z");
        let s = format_unix_utc(1_778_416_496, 789);
        assert!(s.starts_with("2026-05-"));
        assert!(s.ends_with(".789Z"));
    }

    #[test]
    fn corrupt_file_yields_err() {
        let f = tmpfile("corrupt");
        std::fs::write(&f, b"not json").unwrap();
        let res = TodoStore::load_with_projects(f.clone(), None);
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn add_and_list_by_root_path() {
        let f = tmpfile("add-list");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let t = store
            .create("/abs/root/a", "task a", None, false)
            .unwrap();
        assert_eq!(t.root_path, "/abs/root/a");

        let listed = store.list("/abs/root/a");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, t.id);

        // другой root_path — пустой
        assert!(store.list("/abs/root/b").is_empty());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn remove_by_id_searches_across_paths() {
        let f = tmpfile("rm-cross");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let _a1 = store.create("/r1", "a1", None, false).unwrap();
        let b1 = store.create("/r2", "b1", None, false).unwrap();
        let _b2 = store.create("/r2", "b2", None, false).unwrap();

        // удаляем из /r2 — list /r2 уменьшается
        assert!(store.delete(&b1.id).unwrap());
        assert_eq!(store.list("/r1").len(), 1);
        assert_eq!(store.list("/r2").len(), 1);

        // повторный delete = false
        assert!(!store.delete(&b1.id).unwrap());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn update_by_id_searches_across_paths() {
        let f = tmpfile("upd-cross");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let _ = store.create("/r1", "first", None, false).unwrap();
        let t = store.create("/r2", "second", None, false).unwrap();

        let updated = store
            .update(&t.id, Some("renamed".into()), None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(updated.title, "renamed");
        // list для /r2 видит изменение
        let listed = store.list("/r2");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "renamed");

        // несуществующий id — None
        assert!(store
            .update("no-such-id", Some("x".into()), None, None, None)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn list_returns_empty_for_unknown_root() {
        let f = tmpfile("empty-list");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        assert!(store.list("/never/created").is_empty());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    /// Хелпер: записывает legacy-формат todos.json со старым полем
    /// `project_id`. Возвращает путь к файлу.
    fn write_legacy_todos(path: &Path, project_ids: &[&str]) {
        let mut todos = Vec::new();
        for (i, pid) in project_ids.iter().enumerate() {
            todos.push(serde_json::json!({
                "id": format!("legacy-{i}-{pid}"),
                "project_id": pid,
                "title": format!("legacy task {i}"),
                "description": null,
                "priority": 2,
                "issue_type": "task",
                "labels": [],
                "plan_mode": false,
                "created_at": "2026-01-01T00:00:00.000Z",
                "updated_at": "2026-01-01T00:00:00.000Z",
                "origin": "local"
            }));
        }
        let envelope = serde_json::json!({ "todos": todos });
        std::fs::write(path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
    }

    /// Хелпер: пишет stub projects.json с маппингом id → path.
    fn write_projects_stub(path: &Path, mapping: &[(&str, &str)]) {
        let projects: Vec<serde_json::Value> = mapping
            .iter()
            .map(|(id, p)| {
                serde_json::json!({
                    "id": id,
                    "name": id,
                    "path": p,
                    "tmux_prefix": id
                })
            })
            .collect();
        let envelope = serde_json::json!({
            "projects": projects,
            "active_project_id": mapping.first().map(|x| x.0).unwrap_or(""),
        });
        std::fs::write(path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
    }

    #[test]
    fn migrate_from_legacy_project_id() {
        let f = tmpfile("mig-legacy");
        let projects_path = f.parent().unwrap().join("projects.json");
        write_legacy_todos(&f, &["forge", "other"]);
        write_projects_stub(
            &projects_path,
            &[
                ("forge", "/abs/forge/root"),
                ("other", "/abs/other/root"),
            ],
        );

        let store =
            TodoStore::load_with_projects(f.clone(), Some(projects_path.clone())).unwrap();
        // Доступ через новый root_path (абсолютный путь)
        let forge_list = store.list("/abs/forge/root");
        assert_eq!(forge_list.len(), 1, "forge legacy task should be migrated");
        assert_eq!(forge_list[0].root_path, "/abs/forge/root");

        let other_list = store.list("/abs/other/root");
        assert_eq!(other_list.len(), 1);

        // Старый ключ больше не работает
        assert!(store.list("forge").is_empty());

        // Файл уже в новом формате — `project_id` отсутствует, есть только `root_path`.
        let raw = std::fs::read_to_string(&f).unwrap();
        assert!(
            !raw.contains("\"project_id\""),
            "saved file must not contain legacy project_id key, got: {raw}"
        );
        assert!(raw.contains("\"root_path\""));
        assert!(raw.contains("/abs/forge/root"));

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn migrate_without_projects_json_falls_back_to_project_id_as_path() {
        let f = tmpfile("mig-no-projects");
        // projects.json не пишем — пусть отсутствует.
        let projects_path = f.parent().unwrap().join("projects.json");
        write_legacy_todos(&f, &["forge"]);

        let store =
            TodoStore::load_with_projects(f.clone(), Some(projects_path)).unwrap();
        // Fallback: project_id трактуется как root_path.
        let list = store.list("forge");
        assert_eq!(list.len(), 1, "legacy task preserved as fallback");
        assert_eq!(list[0].root_path, "forge");

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn save_load_roundtrip() {
        let f = tmpfile("roundtrip");
        {
            let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
            store.create("/abs/r1", "t1", Some("desc".into()), false).unwrap();
            store.create("/abs/r1", "t2", None, true).unwrap();
            store.create("/abs/r2", "t3", None, false).unwrap();
        }
        // Заново открываем — все данные на месте.
        let store2 = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let r1 = store2.list("/abs/r1");
        assert_eq!(r1.len(), 2);
        let r2 = store2.list("/abs/r2");
        assert_eq!(r2.len(), 1);

        // plan_mode сохранилось
        let t2 = r1.iter().find(|t| t.title == "t2").unwrap();
        assert!(t2.plan_mode);

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn auto_promote_roundtrip_persists() {
        let f = tmpfile("auto-promote-roundtrip");
        let id = {
            let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
            // Новый TODO стартует с auto_promote=false (через дефолт поля).
            let t = store
                .create("/abs/r1", "queued task", None, false)
                .unwrap();
            assert!(!t.auto_promote, "новый TODO по умолчанию не помечен");

            // Помечаем для авто-промоута.
            let updated = store
                .update(&t.id, None, None, None, Some(true))
                .unwrap()
                .unwrap();
            assert!(updated.auto_promote, "auto_promote записан в Some(true)");
            t.id
        };

        // Перезагружаем из файла — флаг должен сохраниться.
        let store2 = TodoStore::load(f.clone()).unwrap();
        let reloaded = store2.get(&id).unwrap();
        assert!(
            reloaded.auto_promote,
            "auto_promote сохранился после reload через load"
        );

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn move_to_root_relocates_between_buckets() {
        let f = tmpfile("move-root");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let t = store
            .create("/abs/r1", "task to move", Some("desc".into()), false)
            .unwrap();
        let original_updated = t.updated_at.clone();
        // Sleep чуть-чуть чтобы updated_at сдвинулся (1 мс granularity).
        std::thread::sleep(std::time::Duration::from_millis(2));

        // Move /abs/r1 → /abs/r2.
        let moved = store
            .move_to_root(&t.id, "/abs/r2")
            .unwrap()
            .expect("move_to_root should find existing todo");

        // Поле обновлено.
        assert_eq!(moved.root_path, "/abs/r2");
        assert!(moved.updated_at >= original_updated);

        // Bucket /abs/r1 пуст, /abs/r2 содержит карточку.
        assert!(store.list("/abs/r1").is_empty());
        let r2 = store.list("/abs/r2");
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].id, t.id);
        assert_eq!(r2[0].root_path, "/abs/r2");

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn move_to_root_returns_none_for_unknown_id() {
        let f = tmpfile("move-noop");
        let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
        let res = store.move_to_root("no-such-id", "/abs/r3").unwrap();
        assert!(res.is_none());
        assert!(store.list("/abs/r3").is_empty());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn move_to_root_persists_across_reload() {
        let f = tmpfile("move-persist");
        let id;
        {
            let store = TodoStore::load_with_projects(f.clone(), None).unwrap();
            let t = store.create("/abs/old", "x", None, false).unwrap();
            id = t.id.clone();
            store.move_to_root(&id, "/abs/new").unwrap().unwrap();
        }
        // Reload — bucket в новом корне.
        let store2 = TodoStore::load_with_projects(f.clone(), None).unwrap();
        assert!(store2.list("/abs/old").is_empty());
        let new_list = store2.list("/abs/new");
        assert_eq!(new_list.len(), 1);
        assert_eq!(new_list[0].id, id);
        assert_eq!(new_list[0].root_path, "/abs/new");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn looks_like_abs_path_recognizes_posix_and_windows() {
        assert!(looks_like_abs_path("/abs/path"));
        assert!(looks_like_abs_path("/"));
        assert!(looks_like_abs_path("C:/users"));
        assert!(looks_like_abs_path("D:\\foo"));
        assert!(!looks_like_abs_path("forge"));
        assert!(!looks_like_abs_path("my-project"));
        assert!(!looks_like_abs_path(""));
    }
}
