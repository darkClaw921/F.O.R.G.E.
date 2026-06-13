//! Глобальный конфиг notifier'а — снимает привязку настроек уведомлений к
//! конкретному проекту (см. план `remove-projects-concept.md`).
//!
//! ### Назначение
//!
//! Раньше `notify_template`, `notify_delay_minutes`, `notify_wait_previous`,
//! `notify_session` хранились в `Project` (см. `projects::Project`). После
//! отказа от концепции «проекта» эти настройки становятся глобальными:
//! один файл на пользователя, применяется к любым promote-операциям из
//! TODO в bd-задачу.
//!
//! ### Хранилище
//!
//! `~/.config/forge/notifier.json` (отдельный файл, **не** встраивается в
//! `user_settings.json`) — чтобы не загружать `user_settings` notifier-
//! специфичными полями. По соглашению с `projects.json` / `themes.json`
//! каталог `~/.config/forge/` уже существует к моменту запуска devforge.
//!
//! ### Дефолты
//!
//! - `template: ""` (пустая строка → в `promote_todo` используется
//!   `DEFAULT_PROMOTE_TEMPLATE` — `[{id}] {title}`, описание агент берёт сам
//!   через `br show <id>`; notify всё равно скипается, только если не
//!   определена целевая сессия).
//! - `delay_minutes: 0` (Immediate).
//! - `wait_previous: false` (без FIFO-очереди по предыдущему promote).
//! - `session: None` (целевая сессия должна приходить из body запроса).
//!
//! ### Persistence
//!
//! Atomic save: tempfile + rename. Идентично [`crate::user_settings`] и
//! [`crate::todos`]. При битом файле падать **нельзя** — devforge должен
//! стартовать с дефолтами и продолжить работать.
//!
//! ### Lazy file creation
//!
//! `NotifierConfigStore::new(path)` НЕ создаёт файл, если его нет. Файл
//! появится только при первом успешном [`NotifierConfigStore::patch`] или
//! [`NotifierConfigStore::put`]. Это сохраняет инвариант «zero-config =
//! состояние по умолчанию».

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Глобальные настройки уведомлений (один экземпляр на пользователя).
///
/// Сериализуется в `~/.config/forge/notifier.json`. Все поля
/// `#[serde(default)]` — частичный/пустой файл считается валидным
/// состоянием (пропущенные поля берутся из [`Default`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotifierConfig {
    /// Шаблон notify-сообщения. Поддерживаемые плейсхолдеры — см.
    /// `format_notify_template` в `main.rs`: `{id}`, `{title}`,
    /// `{description}`, `{priority}`, `{type}`. Пустая строка ⇒ в `promote_todo`
    /// используется `DEFAULT_PROMOTE_TEMPLATE` (`[{id}] {title}` — без описания,
    /// агент подтягивает его через `br show <id>`).
    #[serde(default)]
    pub template: String,
    /// Задержка перед отправкой notify в минутах. `0` ⇒ Immediate.
    #[serde(default)]
    pub delay_minutes: u32,
    /// Если `true` — следующий promote ждёт, пока предыдущий
    /// promoted-issue не закроется (см. `NotifyMode::WaitPrevious`).
    #[serde(default)]
    pub wait_previous: bool,
    /// Дефолтная tmux-сессия, в которую отправлять notify. Если
    /// `None` ⇒ обязателен `body.session` в `promote_todo`. Если
    /// `body.session` задан — он имеет приоритет.
    #[serde(default)]
    pub session: Option<String>,
}

impl Default for NotifierConfig {
    fn default() -> Self {
        Self {
            template: String::new(),
            delay_minutes: 0,
            wait_previous: false,
            session: None,
        }
    }
}

/// DTO для `PATCH /api/notifier-config`. Все поля `Option<T>` — применяются
/// только `Some(..)`-варианты. Это позволяет клиенту менять одно поле, не
/// высылая полный объект.
///
/// Семантика `session: Option<Option<String>>`:
/// - отсутствует / `null` в JSON и `serde(default)` → `None` → поле не меняется;
/// - `Some(None)` (явное `null` через двойную обёртку не нужно — клиент шлёт
///   пустую строку, см. ниже) → не используется здесь;
/// - чтобы стереть session, клиент передаёт пустую строку — она парсится в
///   `Some(String::new())`, и `patch` интерпретирует пустую строку как сброс
///   в `None`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchNotifierConfigReq {
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub delay_minutes: Option<u32>,
    #[serde(default)]
    pub wait_previous: Option<bool>,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug)]
struct Inner {
    cfg: NotifierConfig,
    path: PathBuf,
}

/// In-memory + on-disk хранилище [`NotifierConfig`].
///
/// Cheap-clonable: внутри `Arc<RwLock<Inner>>`. Один экземпляр на процесс —
/// кладётся в `AppState`. Все мутации (`put`, `patch`) проходят atomic save
/// на диск под write-lock'ом.
#[derive(Debug, Clone)]
pub struct NotifierConfigStore {
    inner: Arc<RwLock<Inner>>,
}

impl NotifierConfigStore {
    /// Создаёт store. Если файл существует — пытается прочитать его и
    /// разобрать как [`NotifierConfig`]. При успехе использует разобранное
    /// значение; при ошибке парсинга — печатает warning в tracing и
    /// продолжает с [`NotifierConfig::default`] (политика «битый файл не
    /// блокирует работу»).
    ///
    /// Файл **не создаётся**, если его нет — это нужно для инварианта
    /// «zero-config = поведение по умолчанию».
    pub fn new(path: PathBuf) -> Self {
        let cfg = match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<NotifierConfig>(&body) {
                Ok(c) => {
                    tracing::info!(
                        path = %path.display(),
                        "loaded notifier.json"
                    );
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "failed to parse notifier.json; falling back to defaults"
                    );
                    NotifierConfig::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %path.display(),
                    "notifier.json not found; using defaults"
                );
                NotifierConfig::default()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = ?e,
                    "failed to read notifier.json; falling back to defaults"
                );
                NotifierConfig::default()
            }
        };

        Self {
            inner: Arc::new(RwLock::new(Inner { cfg, path })),
        }
    }

    /// Возвращает клон текущей конфигурации под read-lock'ом.
    pub fn get(&self) -> NotifierConfig {
        let inner = self.inner.read().expect("NotifierConfigStore lock poisoned");
        inner.cfg.clone()
    }

    /// Полная замена конфигурации (`PUT /api/notifier-config`). Сохраняет
    /// на диск atomic save. Возвращает финальный снимок (для удобства
    /// echo-ответа клиенту).
    pub fn put(&self, new_cfg: NotifierConfig) -> Result<NotifierConfig> {
        let (snap, cfg) = {
            let mut inner = self.inner.write().expect("NotifierConfigStore lock poisoned");
            inner.cfg = new_cfg;
            (serialize_locked(&inner)?, inner.cfg.clone())
        };
        write_snapshot(&snap)?;
        Ok(cfg)
    }

    /// Частичный patch (`PATCH /api/notifier-config`). Только поля `Some(..)`
    /// обновляются. После мутации — atomic save. Возвращает обновлённую копию.
    ///
    /// Правила:
    /// - `session = Some("")` ⇒ сброс в `None` (sentinel «убрать дефолт»).
    /// - `session = Some("foo")` ⇒ задать `Some("foo")`.
    /// - `session = None` (поле отсутствует) ⇒ не трогать.
    pub fn patch(&self, payload: PatchNotifierConfigReq) -> Result<NotifierConfig> {
        let mut inner = self.inner.write().expect("NotifierConfigStore lock poisoned");
        if let Some(v) = payload.template {
            inner.cfg.template = v;
        }
        if let Some(v) = payload.delay_minutes {
            inner.cfg.delay_minutes = v;
        }
        if let Some(v) = payload.wait_previous {
            inner.cfg.wait_previous = v;
        }
        if let Some(v) = payload.session {
            // Sentinel: пустая строка ⇒ сброс в None.
            inner.cfg.session = if v.trim().is_empty() { None } else { Some(v) };
        }
        let snap = serialize_locked(&inner)?;
        let cfg = inner.cfg.clone();
        drop(inner);
        write_snapshot(&snap)?;
        Ok(cfg)
    }
}

/// Снимок для записи: сериализованное тело + целевой и tmp-путь. Готовится под
/// lock'ом ([`serialize_locked`]), пишется уже без guard'а ([`write_snapshot`])
/// — чтобы блокирующий fs I/O не держал RwLock.
struct SaveSnapshot {
    body: Vec<u8>,
    path: PathBuf,
    tmp: PathBuf,
}

/// Готовит снимок состояния под write-lock'ом. НЕ трогает диск.
fn serialize_locked(inner: &Inner) -> Result<SaveSnapshot> {
    let body =
        serde_json::to_vec_pretty(&inner.cfg).context("failed to serialize NotifierConfig")?;

    let mut tmp = inner.path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    Ok(SaveSnapshot {
        body,
        path: inner.path.clone(),
        tmp,
    })
}

/// Атомарно пишет снимок на диск БЕЗ удерживаемого lock'а.
///
/// Стратегия идентична `user_settings` и `todos`: пишем в `<file>.tmp`, затем
/// `rename` поверх. На POSIX rename атомарен в рамках одного mount-point.
fn write_snapshot(snap: &SaveSnapshot) -> Result<()> {
    if let Some(parent) = snap.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dir {}", parent.display()))?;
        }
    }

    std::fs::write(&snap.tmp, &snap.body)
        .with_context(|| format!("failed to write tmp {}", snap.tmp.display()))?;
    std::fs::rename(&snap.tmp, &snap.path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            snap.tmp.display(),
            snap.path.display()
        )
    })?;
    Ok(())
}

/// Возвращает дефолтный путь конфига: `~/.config/forge/notifier.json`.
///
/// На случай отсутствия `HOME` — fallback в `std::env::temp_dir()` (как и
/// в `user_settings`), чтобы Store оставался работоспособным, хотя
/// persistence через перезапуск не гарантирован.
pub fn default_config_path() -> PathBuf {
    match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home)
            .join(".config")
            .join("forge")
            .join("notifier.json"),
        Err(_) => std::env::temp_dir().join("devforge_notifier.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(label: &str) -> PathBuf {
        let id = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("devforge_notifier_cfg_{label}_{id}.json"))
    }

    #[test]
    fn default_when_no_file() {
        let path = tmp_path("no_file");
        assert!(!path.exists());
        let store = NotifierConfigStore::new(path.clone());
        let cfg = store.get();
        assert_eq!(cfg, NotifierConfig::default());
        // Файл не создаётся read-only обращением.
        assert!(!path.exists());
    }

    #[test]
    fn put_and_reload() {
        let path = tmp_path("put_reload");
        let store = NotifierConfigStore::new(path.clone());
        let new = NotifierConfig {
            template: "Новая [{id}]: {title}".to_string(),
            delay_minutes: 5,
            wait_previous: true,
            session: Some("forge-main".to_string()),
        };
        let saved = store.put(new.clone()).unwrap();
        assert_eq!(saved, new);

        let store2 = NotifierConfigStore::new(path.clone());
        assert_eq!(store2.get(), new);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn patch_partial() {
        let path = tmp_path("patch_partial");
        let store = NotifierConfigStore::new(path.clone());
        // Базово выставим всё.
        store
            .put(NotifierConfig {
                template: "old".into(),
                delay_minutes: 1,
                wait_previous: false,
                session: Some("s".into()),
            })
            .unwrap();

        // Меняем только template.
        let after = store
            .patch(PatchNotifierConfigReq {
                template: Some("new".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(after.template, "new");
        assert_eq!(after.delay_minutes, 1);
        assert!(!after.wait_previous);
        assert_eq!(after.session.as_deref(), Some("s"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn patch_session_empty_resets_to_none() {
        let path = tmp_path("patch_session_empty");
        let store = NotifierConfigStore::new(path.clone());
        store
            .put(NotifierConfig {
                template: String::new(),
                delay_minutes: 0,
                wait_previous: false,
                session: Some("forge".into()),
            })
            .unwrap();

        let after = store
            .patch(PatchNotifierConfigReq {
                session: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(after.session, None);

        // Persistence.
        let store2 = NotifierConfigStore::new(path.clone());
        assert_eq!(store2.get().session, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_partial_file_loads_with_defaults() {
        // Эмулируем «старый» / частичный файл без некоторых полей.
        let path = tmp_path("legacy_partial");
        let body = r#"{ "template": "hello {id}" }"#;
        std::fs::write(&path, body).unwrap();

        let store = NotifierConfigStore::new(path.clone());
        let cfg = store.get();
        assert_eq!(cfg.template, "hello {id}");
        // Пропущенные поля → дефолты.
        assert_eq!(cfg.delay_minutes, 0);
        assert!(!cfg.wait_previous);
        assert_eq!(cfg.session, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn broken_file_falls_back_to_defaults() {
        let path = tmp_path("broken");
        std::fs::write(&path, "not json at all").unwrap();
        let store = NotifierConfigStore::new(path.clone());
        assert_eq!(store.get(), NotifierConfig::default());
        let _ = std::fs::remove_file(&path);
    }
}
