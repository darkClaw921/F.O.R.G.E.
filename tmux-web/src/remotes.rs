//! Реестр remote-серверов devforge.
//!
//! ## Назначение
//!
//! Локальный devforge может агрегировать несколько remote-инстансов
//! (других devforge'ов, запущенных где-то на других машинах с `--remote`).
//! Этот модуль хранит список таких удалённых серверов: id, человекочитаемая
//! метка, base URL и Bearer-token для авторизации.
//!
//! Файл персиста — `~/.config/forge/remote_servers.json` (тот же каталог,
//! что `projects.json` и `server_config.json`, см. [`cli::state_dir`]).
//!
//! ## Модель
//!
//! - [`RemoteServer`] — одна запись `{ id, label, url, token }`. Поле `token`
//!   *никогда* не отдаётся наружу в API-ответах (для этого есть отдельный
//!   DTO [`RemoteServerView`]). `id` — slug от `label` с авто-дедупликацией
//!   (`-2`, `-3`, ...), стабильный после первой записи.
//! - [`RemoteServerStore`] — in-memory копия + путь к файлу. Не реализует
//!   `Clone` — доступ из axum-state через `Arc<RwLock<...>>`.
//!
//! ## Атомарная запись
//!
//! [`RemoteServerStore::save`] пишет в `<file>.tmp` и делает `rename` поверх,
//! по тому же паттерну, что `projects::ProjectStore::save` и
//! `server_config::save_to`. На POSIX rename атомарен в пределах одного
//! mount-point.
//!
//! ## Где независим от remote_mode
//!
//! Реестр работает ВСЕГДА. Даже когда devforge стартует в legacy localhost,
//! пользователь может через `devforge remote add ...` положить запись о
//! remote-сервере, чтобы потом подключиться к нему. Сами CRUD-эндпоинты
//! (Phase 2 task .3) подключаются только при `remote_mode=true`, но это
//! отдельное решение — store независим.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli;
use crate::projects::slugify;

/// Одна запись о remote-сервере.
///
/// Поля:
/// - `id` — slug от `label`, с дедупликацией суффиксом `-2`, `-3`, ...
///   Стабилен после `add()` — не меняется при переименовании `label` через
///   [`RemoteServerStore::update`].
/// - `label` — человекочитаемая метка (например, "Office laptop").
/// - `url` — base URL, БЕЗ trailing slash (например, `http://192.168.1.5:7331`).
///   Используется как префикс для HTTP-прокси и WS-прокси (Phase 3-4).
/// - `token` — Bearer-token, который devforge будет посылать как
///   `Authorization: Bearer <token>` при обращении к remote. НЕ сериализуется
///   в API-ответы (см. [`RemoteServerView`]).
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteServer {
    pub id: String,
    pub label: String,
    pub url: String,
    pub token: String,
}

/// Кастомный Debug — НЕ выводит token, чтобы он не утекал в логи
/// (`tracing::debug!("{store:?}")`) или panic-сообщения. Вместо token-значения
/// печатается `[REDACTED]`. Закреплено тестом `debug_redacts_token_field`.
impl std::fmt::Debug for RemoteServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServer")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Public DTO для GET/POST/PATCH ответов. Намеренно НЕ включает `token` —
/// токен хранится только на диске и используется внутри прокси-логики.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteServerView {
    pub id: String,
    pub label: String,
    pub url: String,
}

impl From<&RemoteServer> for RemoteServerView {
    fn from(s: &RemoteServer) -> Self {
        Self {
            id: s.id.clone(),
            label: s.label.clone(),
            url: s.url.clone(),
        }
    }
}

/// Envelope для `~/.config/forge/remote_servers.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RemotesFile {
    #[serde(default)]
    servers: Vec<RemoteServer>,
}

/// In-memory реестр + путь к файлу.
///
/// Доступ из axum-state — `Arc<RwLock<RemoteServerStore>>`. Не Clone намеренно.
#[derive(Debug)]
pub struct RemoteServerStore {
    file_path: PathBuf,
    servers: Vec<RemoteServer>,
}

impl RemoteServerStore {
    /// Загружает реестр из файла; при отсутствии — создаёт пустой store
    /// (файл на диске НЕ создаётся до первого `save()`).
    ///
    /// При поломанном JSON — возвращает Err.
    pub fn load(file_path: PathBuf) -> Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config dir {}", parent.display())
            })?;
        }

        if !file_path.exists() {
            tracing::debug!(path = %file_path.display(), "remote_servers.json missing — empty store");
            return Ok(Self {
                file_path,
                servers: Vec::new(),
            });
        }

        let raw = std::fs::read(&file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        let parsed: RemotesFile = serde_json::from_slice(&raw)
            .with_context(|| format!("failed to parse {}", file_path.display()))?;

        Ok(Self {
            file_path,
            servers: parsed.servers,
        })
    }

    /// Атомарно сохраняет реестр в `self.file_path`.
    ///
    /// Стратегия: пишем в `<file>.tmp`, затем `rename` поверх старого.
    /// На POSIX rename атомарен в пределах одного mount-point. Совпадает с
    /// паттерном `projects::ProjectStore::save` и `server_config::save_to`.
    pub fn save(&self) -> Result<()> {
        let envelope = RemotesFile {
            servers: self.servers.clone(),
        };
        let body = serde_json::to_vec_pretty(&envelope)
            .context("failed to serialize RemotesFile")?;

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

    /// Возвращает копию списка серверов.
    pub fn list(&self) -> Vec<RemoteServer> {
        self.servers.clone()
    }

    /// Public-view список (без токенов) — удобно отдавать из REST.
    pub fn list_views(&self) -> Vec<RemoteServerView> {
        self.servers.iter().map(RemoteServerView::from).collect()
    }

    /// Возвращает ссылку на сервер по id.
    pub fn get(&self, id: &str) -> Option<&RemoteServer> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// Добавляет новый сервер. `id` генерится из `label` через
    /// [`crate::projects::slugify`] с авто-дедупликацией: если `slug` уже
    /// занят — приписывает `-2`, `-3`, ... до свободного.
    ///
    /// Возвращает добавленный [`RemoteServer`] (с финальным `id`). Disk
    /// persistence — caller вызывает [`save`] явно после `add`.
    ///
    /// Ошибки:
    /// - пустая `label` → bail.
    /// - пустой `url` → bail.
    /// - URL должен начинаться с `http://` или `https://`.
    /// - пустой `token` → bail.
    pub fn add(
        &mut self,
        label: impl Into<String>,
        url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<RemoteServer> {
        let label = label.into();
        let url = url.into();
        let token = token.into();

        if label.trim().is_empty() {
            bail!("remote server `label` is required");
        }
        if url.trim().is_empty() {
            bail!("remote server `url` is required");
        }
        if !is_valid_remote_url(&url) {
            bail!("remote server `url` must start with http:// or https://");
        }
        if token.trim().is_empty() {
            bail!("remote server `token` is required");
        }

        let id = self.allocate_id(&label)?;
        let url_normalized = trim_trailing_slash(&url).to_string();
        let server = RemoteServer {
            id: id.clone(),
            label,
            url: url_normalized,
            token,
        };
        self.servers.push(server.clone());
        Ok(server)
    }

    /// Удаляет сервер по id. Возвращает `true` если действительно удалили
    /// (запись существовала), иначе `false`.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|s| s.id != id);
        self.servers.len() != before
    }

    /// Обновляет label и/или token у существующей записи. `id` и `url`
    /// неизменяемы — для смены URL нужно `remove + add`.
    ///
    /// `None` для поля — не трогать; `Some(...)` — записать.
    /// Возвращает `Some(updated)` или `None` если id неизвестен.
    pub fn update(
        &mut self,
        id: &str,
        label: Option<String>,
        token: Option<String>,
    ) -> Option<RemoteServer> {
        let server = self.servers.iter_mut().find(|s| s.id == id)?;
        if let Some(l) = label {
            if !l.trim().is_empty() {
                server.label = l;
            }
        }
        if let Some(t) = token {
            if !t.trim().is_empty() {
                server.token = t;
            }
        }
        Some(server.clone())
    }

    /// Внутренний генератор id с дедупликацией.
    fn allocate_id(&self, label: &str) -> Result<String> {
        let base = slugify(label);
        if base.is_empty() {
            return Err(anyhow!("label `{label}` produced empty slug"));
        }
        if self.get(&base).is_none() {
            return Ok(base);
        }
        // Подбираем `-2`, `-3`, ... пока не свободен. Цикл всегда сходится
        // (множество id конечно, новых не создаётся параллельно — store под
        // write-lock'ом).
        for suffix in 2..=u32::MAX {
            let candidate = format!("{base}-{suffix}");
            if self.get(&candidate).is_none() {
                return Ok(candidate);
            }
        }
        Err(anyhow!("could not allocate unique id for label `{label}`"))
    }
}

/// `~/.config/forge/remote_servers.json` (тот же каталог, что projects.json
/// и server_config.json — через [`cli::state_dir`]).
pub fn default_remotes_path() -> Result<PathBuf> {
    Ok(cli::state_dir()?.join("remote_servers.json"))
}

/// Проверка, что URL начинается с http:// или https://. Минимальная
/// валидация — полноценный парсинг URL не нужен на этом уровне, реальные
/// ошибки всплывут при первом HTTP-запросе через reqwest (Phase 3).
pub fn is_valid_remote_url(url: &str) -> bool {
    let s = url.trim();
    s.starts_with("http://") || s.starts_with("https://")
}

/// Убирает trailing slash у URL чтобы не плодить двойные `//` при склейке
/// `<url>/healthz`. Если slash'а нет — возвращает как есть.
pub fn trim_trailing_slash(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-remotes-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_missing_returns_empty() {
        let dir = tempdir("missing");
        let file = dir.join("remote_servers.json");
        let store = RemoteServerStore::load(file.clone()).unwrap();
        assert_eq!(store.list().len(), 0);
        assert!(!file.exists(), "missing file should NOT be created on load");
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir("rt");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file.clone()).unwrap();
        store
            .add("Office", "http://192.168.1.5:7331", "tok-1")
            .unwrap();
        store.save().unwrap();

        let store2 = RemoteServerStore::load(file).unwrap();
        let list = store2.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "office");
        assert_eq!(list[0].label, "Office");
        assert_eq!(list[0].url, "http://192.168.1.5:7331");
        assert_eq!(list[0].token, "tok-1");
    }

    #[test]
    fn add_then_remove() {
        let dir = tempdir("rm");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();

        let s = store.add("Home", "http://10.0.0.2:7331", "tk").unwrap();
        assert_eq!(s.id, "home");
        assert!(store.get("home").is_some());

        assert!(store.remove("home"));
        assert!(store.get("home").is_none());
        assert!(!store.remove("home"), "second remove → false");
    }

    #[test]
    fn update_label_and_token_keeps_id() {
        let dir = tempdir("upd");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let s = store
            .add("Old Label", "http://x:7331", "tk-old")
            .unwrap();
        let old_id = s.id.clone();

        let updated = store
            .update(
                &old_id,
                Some("New Label".to_string()),
                Some("tk-new".to_string()),
            )
            .unwrap();
        assert_eq!(updated.id, old_id, "id must remain unchanged");
        assert_eq!(updated.label, "New Label");
        assert_eq!(updated.token, "tk-new");
    }

    #[test]
    fn update_unknown_id_returns_none() {
        let dir = tempdir("upd-none");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let got = store.update("nope", Some("x".into()), None);
        assert!(got.is_none());
    }

    #[test]
    fn slugify_collision_appends_suffix() {
        let dir = tempdir("collision");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();

        let a = store.add("Office", "http://a:7331", "tk-a").unwrap();
        let b = store.add("Office", "http://b:7331", "tk-b").unwrap();
        let c = store.add("Office", "http://c:7331", "tk-c").unwrap();
        assert_eq!(a.id, "office");
        assert_eq!(b.id, "office-2");
        assert_eq!(c.id, "office-3");
    }

    #[test]
    fn add_rejects_invalid_input() {
        let dir = tempdir("invalid");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();

        assert!(store.add("", "http://x", "tk").is_err());
        assert!(store.add("L", "", "tk").is_err());
        assert!(store.add("L", "ftp://x", "tk").is_err());
        assert!(store.add("L", "http://x", "").is_err());
    }

    #[test]
    fn add_trims_trailing_slash() {
        let dir = tempdir("slash");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let s = store
            .add("X", "http://example.com:7331/", "tk")
            .unwrap();
        assert_eq!(s.url, "http://example.com:7331");
    }

    #[test]
    fn view_excludes_token() {
        let dir = tempdir("view");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        store
            .add("Office", "http://a:7331", "super-secret")
            .unwrap();
        let views = store.list_views();
        assert_eq!(views.len(), 1);
        let json = serde_json::to_string(&views[0]).unwrap();
        assert!(!json.contains("super-secret"), "token must NOT leak into JSON: {json}");
        assert!(!json.contains("token"), "token field must not appear: {json}");
    }

    #[test]
    fn atomic_save_via_tempfile_rename() {
        // Косвенный тест: после save в каталоге нет *.tmp файла, а основной
        // файл содержит то, что мы сохранили. Это говорит, что save прошёл
        // через rename (если бы был open+write+close, то tmp файла бы не
        // существовало вовсе — но это не показатель). Главная гарантия —
        // что save завершается успешно и итоговый файл валидный JSON.
        let dir = tempdir("atomic");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file.clone()).unwrap();
        store.add("A", "http://a:7331", "tk").unwrap();
        store.save().unwrap();

        // Tmp файла не осталось.
        let tmp = file.with_file_name("remote_servers.json.tmp");
        assert!(!tmp.exists(), "tmp file must be renamed away after save");

        // Основной файл валиден.
        let raw = std::fs::read_to_string(&file).unwrap();
        let parsed: RemotesFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].id, "a");
    }

    #[test]
    fn is_valid_remote_url_matrix() {
        assert!(is_valid_remote_url("http://x"));
        assert!(is_valid_remote_url("https://x"));
        assert!(is_valid_remote_url("  http://x  "));
        assert!(!is_valid_remote_url("ftp://x"));
        assert!(!is_valid_remote_url("x"));
        assert!(!is_valid_remote_url(""));
    }

    // =========================================================================
    // Phase 8 .8 — Slugify edge cases + Token redaction + Broken JSON
    // =========================================================================

    #[test]
    fn slugify_collision_more_than_three() {
        // 100 серверов с label "Office" → office, office-2, ..., office-100.
        let dir = tempdir("collision-100");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let mut ids = std::collections::HashSet::new();
        for i in 0..100 {
            let s = store
                .add("Office", &format!("http://host-{i}:7331"), "tk")
                .unwrap();
            ids.insert(s.id.clone());
        }
        assert_eq!(ids.len(), 100, "все 100 id должны быть уникальны");
        assert!(ids.contains("office"));
        assert!(ids.contains("office-2"));
        assert!(ids.contains("office-100"));
    }

    #[test]
    fn slugify_unicode_only_label_falls_back_or_transliterates() {
        // Текущая реализация slugify (см. projects::slugify) удаляет все
        // не-ascii символы. Для "Офис" получится пустой slug → allocate_id
        // вернёт Err. Тест-as-spec: документируем поведение.
        let dir = tempdir("unicode");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let r = store.add("Офис", "http://x:7331", "tk");
        // Поведение текущей реализации — Err для unicode-only.
        // Если в будущем добавим транслитерацию, этот тест станет позитивным.
        assert!(
            r.is_err(),
            "unicode-only label на текущей реализации даёт Err (пустой slug)"
        );
    }

    #[test]
    fn slugify_long_label_truncates_to_reasonable_size() {
        // Label > 256 символов → slug имеет разумный предел.
        // Реализация projects::slugify не имеет явного truncate — закрепим
        // фактическое поведение: slug может быть длинным, но не падает.
        let dir = tempdir("long");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        let long_label: String = std::iter::repeat('a').take(500).collect();
        let s = store
            .add(&long_label, "http://x:7331", "tk")
            .expect("длинный label принимается");
        // Slug не должен паниковать при длинном label.
        assert!(!s.id.is_empty());
        // Slug должен быть alphanumeric/dash (валидный URL-segment).
        assert!(s.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn debug_redacts_token_field() {
        let server = RemoteServer {
            id: "office".into(),
            label: "Office".into(),
            url: "http://x".into(),
            token: "SUPER-SECRET-TOKEN-MUST-NOT-LEAK".into(),
        };
        let debug = format!("{server:?}");
        assert!(
            !debug.contains("SUPER-SECRET-TOKEN-MUST-NOT-LEAK"),
            "Debug-форматтер НЕ должен содержать token: {debug}"
        );
        assert!(
            debug.contains("[REDACTED]"),
            "ожидаем `[REDACTED]` placeholder в Debug: {debug}"
        );
    }

    #[test]
    fn store_debug_does_not_leak_tokens() {
        let dir = tempdir("debug-leak");
        let file = dir.join("remote_servers.json");
        let mut store = RemoteServerStore::load(file).unwrap();
        store
            .add("Office", "http://x:7331", "do-not-leak-me-please")
            .unwrap();
        let debug = format!("{store:?}");
        assert!(
            !debug.contains("do-not-leak-me-please"),
            "store Debug не должен светить token: {debug}"
        );
    }

    #[test]
    fn load_broken_json_returns_err_does_not_panic() {
        let dir = tempdir("broken-json");
        let file = dir.join("remote_servers.json");
        std::fs::write(&file, b"{ broken json :((").unwrap();
        let r = RemoteServerStore::load(file.clone());
        assert!(r.is_err(), "broken JSON должен Err");
        // Сообщение об ошибке должно содержать file path.
        let msg = format!("{:#}", r.unwrap_err());
        assert!(
            msg.contains(&file.display().to_string()),
            "err должен упомянуть путь: {msg}"
        );
    }
}
