//! Multi-project registry для tmux-web.
//!
//! ### Назначение
//!
//! Хранит список проектов (имя, корневой путь, tmux-префикс) и id активного.
//! Реестр персистится в `~/.config/forge/projects.json` (atomic write через
//! tempfile + rename). При первом старте файла нет — создаём дефолт с одним
//! проектом `forge`, у которого `path = std::env::current_dir()` и
//! `tmux_prefix = "forge"`.
//!
//! ### Модель
//!
//! - `Project { id, name, path, tmux_prefix }` — один проект. `id` — slug от
//!   имени (только `[a-z0-9_-]+`), уникален. `tmux_prefix` определяет
//!   фильтрацию tmux-сессий (см. 6.B.4): сессия принадлежит проекту, если
//!   её имя — ровно `<prefix>` или начинается с `<prefix>-`. Пустой prefix
//!   означает "все сессии".
//! - `ProjectStore` — in-memory копия + путь к файлу.
//!
//! ### Атомарная запись
//!
//! `save()` пишет в `<file>.tmp` + делает `rename(<file>.tmp, <file>)`. На
//! POSIX это атомарно для одного и того же mount-point — реестр не разъедется
//! даже при kill -9 во время записи.
//!
//! ### Сериализация
//!
//! Файловый формат — JSON envelope `{ projects: [...], active_project_id }`.
//! Кавычки в `path` — как обычная UTF-8 строка (лишь POSIX-пути; на Windows
//! не тестировали, цель — macOS/Linux).

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

/// Описание одного проекта.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    /// Уникальный идентификатор. Slug от `name`: `[a-z0-9_-]+`.
    pub id: String,
    /// Человекочитаемое имя (может содержать пробелы/любые символы).
    pub name: String,
    /// Абсолютный путь к корню проекта. Туда уходят `tmux new-session -c`
    /// и `br list` для tasks.
    pub path: PathBuf,
    /// Префикс tmux-сессий (по умолчанию = `id`). Используется для
    /// фильтрации `/api/sessions` и автопрефиксования при создании.
    pub tmux_prefix: String,

    /// Шаблон текста, отправляемого в tmux при `promote` TODO-карточки.
    /// Поддерживает плейсхолдеры `{title}`, `{description}` и др.
    /// Пустая строка = не отправлять. По умолчанию пусто.
    #[serde(default)]
    pub notify_template: String,
    /// Задержка перед отправкой нотификации в минутах. `0` = немедленно.
    #[serde(default)]
    pub notify_delay_minutes: u32,
    /// Если `true` — notifier ждёт закрытия предыдущей tmux-задачи
    /// (через `tasks_watcher`) прежде чем отправить следующую.
    #[serde(default)]
    pub notify_wait_previous: bool,
    /// Override имени tmux-сессии для нотификаций. Если `None` —
    /// используется сессия активного проекта (по `tmux_prefix`).
    #[serde(default)]
    pub notify_session: Option<String>,
}

/// Файловый envelope для `~/.config/forge/projects.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectsFile {
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    active_project_id: String,
}

/// Реестр проектов в памяти + путь к файлу.
///
/// Не реализует `Clone` намеренно — чтобы случайно не плодить расходящиеся
/// копии. Доступ из axum-state — через `Arc<RwLock<ProjectStore>>`.
///
/// `transient_active` — синтетический проект для auto-group сессий
/// (нерегистрированный cwd). Когда `Some`, перекрывает registered active в
/// `active()`/`active_id()`. Не сериализуется в `projects.json`.
#[derive(Debug)]
pub struct ProjectStore {
    file_path: PathBuf,
    projects: Vec<Project>,
    active_id: String,
    transient_active: Option<Project>,
}

impl ProjectStore {
    /// Загружает реестр из файла; при отсутствии — создаёт дефолт и
    /// записывает на диск.
    ///
    /// `file_path` — путь к JSON-файлу. Каталоги создаются при необходимости.
    /// Дефолтный проект — `forge` с `path = std::env::current_dir()`.
    pub fn load(file_path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config dir {}", parent.display())
            })?;
        }

        if !file_path.exists() {
            tracing::info!(path = %file_path.display(), "projects.json missing — bootstrapping default");
            let default = Self::bootstrap_default(file_path.clone())?;
            default.save()?;
            return Ok(default);
        }

        let raw = std::fs::read(&file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        let parsed: ProjectsFile = serde_json::from_slice(&raw)
            .with_context(|| format!("failed to parse {}", file_path.display()))?;

        // Sanity: если active_project_id указывает на несуществующий — берём
        // первый, либо bootstrap-им дефолт, чтобы инвариант active() не падал.
        let mut store = Self {
            file_path,
            projects: parsed.projects,
            active_id: parsed.active_project_id,
            transient_active: None,
        };

        if store.projects.is_empty() {
            tracing::warn!("projects.json was empty — re-bootstrapping default");
            let default_proj = default_project()?;
            store.active_id = default_proj.id.clone();
            store.projects.push(default_proj);
            store.save()?;
        } else if store.get(&store.active_id).is_none() {
            // active_id потерян — выбираем первый.
            let first_id = store.projects[0].id.clone();
            tracing::warn!(
                old = %store.active_id,
                new = %first_id,
                "active_project_id stale — falling back to first"
            );
            store.active_id = first_id;
            store.save()?;
        }

        Ok(store)
    }

    /// Создаёт пустой store с одним дефолтным проектом, не сохраняя на диск.
    fn bootstrap_default(file_path: PathBuf) -> anyhow::Result<Self> {
        let proj = default_project()?;
        let active_id = proj.id.clone();
        Ok(Self {
            file_path,
            projects: vec![proj],
            active_id,
            transient_active: None,
        })
    }

    /// Атомарно сохраняет реестр в `self.file_path`.
    ///
    /// Стратегия: пишем в `<file>.tmp`, fsync, затем `rename` поверх старого.
    /// На POSIX rename атомарен в пределах одного mount-point.
    pub fn save(&self) -> anyhow::Result<()> {
        let envelope = ProjectsFile {
            projects: self.projects.clone(),
            active_project_id: self.active_id.clone(),
        };
        let body = serde_json::to_vec_pretty(&envelope)
            .context("failed to serialize ProjectsFile")?;

        let mut tmp = self.file_path.clone();
        let mut tmp_name = tmp
            .file_name()
            .map(|s| s.to_owned())
            .unwrap_or_default();
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);

        std::fs::write(&tmp, &body)
            .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file_path).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp.display(),
                self.file_path.display()
            )
        })?;
        Ok(())
    }

    /// Возвращает копию списка проектов (чтобы можно было отдавать наружу
    /// без удержания read-lock'а).
    pub fn list(&self) -> Vec<Project> {
        self.projects.clone()
    }

    /// Возвращает ссылку на проект по id.
    pub fn get(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Ищет проект среди registered + transient_active. Используется когда
    /// потребитель (например `promote_todo`) принимает project_id из
    /// сохранённой сущности, которая могла быть создана под transient
    /// проектом (id вида `__path__:<abs-path>`).
    pub fn find_any(&self, id: &str) -> Option<&Project> {
        if let Some(t) = &self.transient_active {
            if t.id == id {
                return Some(t);
            }
        }
        self.get(id)
    }

    /// Возвращает ссылку на активный проект. Если установлен transient
    /// (через [`set_transient_active`]) — возвращает его, иначе registered.
    pub fn active(&self) -> &Project {
        if let Some(t) = &self.transient_active {
            return t;
        }
        self.get(&self.active_id)
            .expect("invariant: active_id must reference an existing project")
    }

    /// Id активного проекта (transient если установлен).
    pub fn active_id(&self) -> &str {
        if let Some(t) = &self.transient_active {
            return &t.id;
        }
        &self.active_id
    }

    /// Id зарегистрированного активного проекта (игнорирует transient).
    /// Используется для UI-отображения select'а проектов.
    pub fn registered_active_id(&self) -> &str {
        &self.active_id
    }

    /// Устанавливает transient active project — синтетический проект,
    /// указывающий на произвольный cwd (без регистрации в реестре).
    /// Используется для auto-group tmux-сессий (cwd != ни одного registered
    /// project.path). Имя — basename пути, prefix пустой.
    pub fn set_transient_active(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unregistered".to_string());
        let id = format!("__path__:{}", path.display());
        self.transient_active = Some(Project {
            id,
            name,
            path,
            tmux_prefix: String::new(),
            ..Default::default()
        });
    }

    /// Сбрасывает transient active — возвращает фокус на registered active.
    pub fn clear_transient_active(&mut self) {
        self.transient_active = None;
    }

    /// Добавляет новый проект. id = slug(name). Если уже есть проект с таким
    /// id — `Err`.
    ///
    /// `tmux_prefix` опционально: если `None` — берётся равным `id`.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        path: PathBuf,
        tmux_prefix: Option<String>,
    ) -> anyhow::Result<Project> {
        let name = name.into();
        let id = slugify(&name);
        if id.is_empty() {
            bail!("project name `{}` produced empty slug", name);
        }
        if self.get(&id).is_some() {
            bail!("project with id `{}` already exists", id);
        }
        let prefix = tmux_prefix.unwrap_or_else(|| id.clone());
        let proj = Project {
            id: id.clone(),
            name,
            path,
            tmux_prefix: prefix,
            ..Default::default()
        };
        self.projects.push(proj.clone());
        Ok(proj)
    }

    /// Удаляет проект по id. Запрещено удалять активный (вызывающий должен
    /// явно переключить активный заранее).
    pub fn remove(&mut self, id: &str) -> anyhow::Result<bool> {
        if id == self.active_id {
            bail!("cannot remove the active project `{}`", id);
        }
        let before = self.projects.len();
        self.projects.retain(|p| p.id != id);
        Ok(self.projects.len() != before)
    }

    /// Переключает активный проект.
    pub fn set_active(&mut self, id: &str) -> anyhow::Result<()> {
        if self.get(id).is_none() {
            return Err(anyhow!("no project with id `{}`", id));
        }
        self.active_id = id.to_string();
        Ok(())
    }

    /// Обновляет notify-настройки проекта.
    ///
    /// Все параметры опциональны: `None` — не трогать поле; `Some(...)` —
    /// записать новое значение. `Some(None)` для `notify_session` — стереть
    /// (записать `None`). Возвращает обновлённый клон проекта или `Ok(None)`,
    /// если проекта с таким `id` нет.
    ///
    /// Сохранение на диск НЕ выполняется автоматически — caller вызывает
    /// `save()` явно (так же, как `add` / `set_active`).
    pub fn update_settings(
        &mut self,
        id: &str,
        notify_template: Option<String>,
        notify_delay_minutes: Option<u32>,
        notify_wait_previous: Option<bool>,
        notify_session: Option<Option<String>>,
    ) -> Option<Project> {
        let proj = self.projects.iter_mut().find(|p| p.id == id)?;
        if let Some(t) = notify_template {
            proj.notify_template = t;
        }
        if let Some(d) = notify_delay_minutes {
            proj.notify_delay_minutes = d;
        }
        if let Some(w) = notify_wait_previous {
            proj.notify_wait_previous = w;
        }
        if let Some(s) = notify_session {
            proj.notify_session = s;
        }
        Some(proj.clone())
    }
}

/// Конструктор дефолтного проекта `forge` с `path = current_dir`.
fn default_project() -> anyhow::Result<Project> {
    let cwd = std::env::current_dir().context("cannot resolve current_dir")?;
    Ok(Project {
        id: "forge".to_string(),
        name: "forge".to_string(),
        path: cwd,
        tmux_prefix: "forge".to_string(),
        ..Default::default()
    })
}

/// Возвращает путь к `~/.config/forge/projects.json` (используя `$HOME`).
///
/// Не использует крейт `dirs`, чтобы не тянуть лишнюю зависимость — для
/// macOS/Linux достаточно `$HOME`.
pub fn default_registry_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var is not set")?;
    Ok(PathBuf::from(home).join(".config/forge/projects.json"))
}

/// Slug-ификация имени проекта в id.
///
/// Правила:
/// - lower-case;
/// - `[a-z0-9_-]` оставляем как есть;
/// - всё остальное (пробелы, юникод, `/`, `:`) → `-`;
/// - схлопываем подряд идущие `-`;
/// - триммим `-` по краям.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            last_dash = false;
        } else if c == '-' {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        } else {
            // любая «странная» штука — превращаем в `-`, схлопывая повторы.
            if !last_dash && !out.is_empty() {
                out.push('-');
                last_dash = true;
            }
        }
    }
    // тримим `-` по краям.
    let trimmed = out.trim_matches('-').to_string();
    trimmed
}

/// Возвращает ссылку на проект, чей tmux_prefix матчит сессию.
///
/// Сессия принадлежит проекту, если её имя:
/// - ровно равно prefix, или
/// - начинается с `prefix-`.
///
/// Пустой prefix матчит всё.
pub fn session_belongs(prefix: &str, session_name: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if session_name == prefix {
        return true;
    }
    session_name
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('-'))
}

/// Префиксует имя сессии префиксом проекта если это ещё не сделано.
///
/// - Пустой prefix → имя как есть.
/// - Если имя уже начинается с `prefix` или `prefix-` → как есть.
/// - Иначе возвращает `<prefix>-<name>`.
pub fn ensure_prefixed(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        return name.to_string();
    }
    if name == prefix || name.starts_with(&format!("{prefix}-")) {
        return name.to_string();
    }
    format!("{prefix}-{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Process-wide lock for tests that mutate `std::env::set_current_dir`.
    /// `cargo test` runs tests in parallel — without serialization tests race
    /// on cwd and `default_project()` (which reads `current_dir()`) reads a
    /// path that may not exist by the time bootstrap_default builds Project.
    fn cwd_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        match LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-projects-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("forge"), "forge");
        assert_eq!(slugify("My Project"), "my-project");
        assert_eq!(slugify("Foo/Bar:Baz"), "foo-bar-baz");
        assert_eq!(slugify("---a---b---"), "a-b");
        assert_eq!(slugify("UPPER"), "upper");
        assert_eq!(slugify("with_underscore"), "with_underscore");
    }

    #[test]
    fn session_belongs_rules() {
        assert!(session_belongs("", "anything"));
        assert!(session_belongs("forge", "forge"));
        assert!(session_belongs("forge", "forge-x"));
        assert!(!session_belongs("forge", "forgex"));
        assert!(!session_belongs("forge", "other"));
    }

    #[test]
    fn ensure_prefixed_rules() {
        assert_eq!(ensure_prefixed("", "x"), "x");
        assert_eq!(ensure_prefixed("forge", "forge"), "forge");
        assert_eq!(ensure_prefixed("forge", "forge-foo"), "forge-foo");
        assert_eq!(ensure_prefixed("forge", "foo"), "forge-foo");
    }

    #[test]
    fn load_save_roundtrip() {
        let _guard = cwd_lock();
        let dir = tempdir("rt");
        let file = dir.join("projects.json");

        // Меняем cwd на dir, чтобы дефолтный bootstrap не утянул реальный pwd.
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let mut store = ProjectStore::load(file.clone()).unwrap();
        // bootstrap — один проект, активный.
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.active().id, "forge");
        assert!(StdPath::new(&store.active().path).exists());

        // add + save + reload.
        let added = store
            .add(
                "Other Project",
                PathBuf::from("/tmp/forge-projects-test"),
                None,
            )
            .unwrap();
        assert_eq!(added.id, "other-project");
        assert_eq!(added.tmux_prefix, "other-project");

        store.save().unwrap();
        let store2 = ProjectStore::load(file).unwrap();
        assert_eq!(store2.list().len(), 2);
        assert!(store2.get("other-project").is_some());
        assert_eq!(store2.active().id, "forge");

        std::env::set_current_dir(prev_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_active_and_remove() {
        let _guard = cwd_lock();
        let dir = tempdir("active");
        let file = dir.join("projects.json");
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let mut store = ProjectStore::load(file).unwrap();
        store
            .add("X", PathBuf::from("/tmp/forge-x"), Some("xp".into()))
            .unwrap();

        // remove активного — нельзя.
        assert!(store.remove("forge").is_err());

        // переключить и удалить старый — можно.
        store.set_active("x").unwrap();
        assert_eq!(store.active().id, "x");
        assert_eq!(store.active().tmux_prefix, "xp");
        assert!(store.remove("forge").unwrap());
        assert_eq!(store.list().len(), 1);

        // несуществующий id — ошибка.
        assert!(store.set_active("nope").is_err());

        std::env::set_current_dir(prev_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_duplicate_rejected() {
        let _guard = cwd_lock();
        let dir = tempdir("dup");
        let file = dir.join("projects.json");
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let mut store = ProjectStore::load(file).unwrap();
        // bootstrap уже создал `forge` → попытка добавить ещё раз — ошибка.
        let err = store.add("forge", PathBuf::from("/tmp/x"), None);
        assert!(err.is_err());

        std::env::set_current_dir(prev_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
