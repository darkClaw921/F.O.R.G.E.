//! Конфигурация плагина Telegram.
//!
//! Цепочка приоритетов повторяет [`crate::config`]-эталон Echo
//! (`plugins/echo/src/config.rs`):
//!
//! 1. [`TelegramConfig::default`] — статические дефолты.
//! 2. [`TelegramConfig::load_from_file`] — `~/.config/forge/telegram.toml`
//!    (путь переопределяется `FORGE_TELEGRAM_CONFIG_PATH`, см. [`config_path`]),
//!    если файл есть; отсутствие файла — НЕ ошибка, битый TOML — warn + дефолты.
//! 3. [`TelegramConfig::apply_env`] — env-override `FORGE_TELEGRAM_*`:
//!    - `FORGE_TELEGRAM_API_ID`
//!    - `FORGE_TELEGRAM_API_HASH`
//!    - `FORGE_TELEGRAM_SESSION_PATH`
//!    - `FORGE_TELEGRAM_CACHE_DIR`
//!    - `FORGE_TELEGRAM_MAX_UPLOAD_BYTES`
//!    - `FORGE_TELEGRAM_MAX_CACHE_BYTES`
//!    - `FORGE_TELEGRAM_CATCH_UP`
//!    - `FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT`
//! 4. [`TelegramConfig::validate`] / [`TelegramConfig::validate_and_fix`] —
//!    проверка инвариантов и создание родительских каталогов.
//!
//! [`TelegramConfig::load`] делает всё вместе и НИКОГДА не паникует: любая
//! проблема (нет файла, битый TOML, мусор в env, невалидное значение) — это
//! warn-лог и откат на дефолт.
//!
//! `api_id` / `api_hash` намеренно `Option`: пользователь вводит их в UI
//! (см. login-wizard, Фаза 2), поэтому конфиг обязан быть валидным и без них.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Дефолтный путь TOML-конфига плагина (`~/.config/forge/telegram.toml`).
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/telegram.toml")
    } else {
        PathBuf::from("./telegram.toml")
    }
}

/// Фактический путь TOML-конфига: непустой `FORGE_TELEGRAM_CONFIG_PATH`
/// побеждает, иначе — [`default_config_path`].
///
/// Env-override нужен контейнерам/systemd-юнитам с нестандартным layout'ом
/// и герметичным тестам: без него [`TelegramConfig::load`] всегда читал бы
/// реальный `~/.config/forge/telegram.toml` машины разработчика.
/// [`TelegramConfig::save_credentials`] пишет по этому же пути — load и save
/// всегда работают с одним файлом.
pub fn config_path() -> PathBuf {
    match std::env::var_os("FORGE_TELEGRAM_CONFIG_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_config_path(),
    }
}

/// Дефолтный путь к файлу MTProto-сессии (`~/.config/forge/telegram.session`).
///
/// Файл содержит auth-key аккаунта — это фактически ключ от переписки,
/// поэтому Фаза 2 пишет его атомарно и с правами `0600`.
pub fn default_session_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/telegram.session")
    } else {
        PathBuf::from("./telegram.session")
    }
}

/// Дефолтный каталог файлового кэша медиа (`~/.forge/telegram-cache/`).
pub fn default_cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge/telegram-cache")
    } else {
        PathBuf::from("./telegram-cache")
    }
}

/// Максимальный размер одного исходящего файла — 2 GiB (лимит Telegram для
/// обычных аккаунтов; Premium умеет 4 GiB, но мы не полагаемся на него).
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Верхняя граница файлового кэша медиа — 512 MiB. LRU-sweeper (Фаза 4)
/// подчищает по mtime до этого значения.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Глубина очереди апдейтов от MTProto-раннера. При переполнении апдейты
/// дропаются, а клиенту уходит `resync` — так медленный WS-подписчик не
/// приводит к неограниченному росту памяти.
pub const DEFAULT_UPDATE_QUEUE_LIMIT: usize = 1024;

/// Догонять ли пропущенные апдейты после реконнекта (grammers `catch_up`).
pub const DEFAULT_CATCH_UP: bool = true;

fn default_max_upload_bytes() -> u64 {
    DEFAULT_MAX_UPLOAD_BYTES
}

fn default_max_cache_bytes() -> u64 {
    DEFAULT_MAX_CACHE_BYTES
}

fn default_catch_up() -> bool {
    DEFAULT_CATCH_UP
}

fn default_update_queue_limit() -> usize {
    DEFAULT_UPDATE_QUEUE_LIMIT
}

/// Полная конфигурация плагина Telegram.
///
/// Cheap-clonable (все поля по значению). Продакшен собирает её через
/// [`TelegramConfig::load`], тесты — через `TelegramConfig::default()` с
/// точечным переопределением полей.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// `api_id` приложения с <https://my.telegram.org>. `None` ⇒ плагин в
    /// состоянии [`crate::login::LoginFlow::NotConfigured`] и логин недоступен.
    #[serde(default)]
    pub api_id: Option<i32>,
    /// `api_hash` того же приложения. Пустая строка невалидна (см.
    /// [`TelegramConfig::validate`]).
    #[serde(default)]
    pub api_hash: Option<String>,
    /// Путь к файлу MTProto-сессии.
    #[serde(default = "default_session_path")]
    pub session_path: PathBuf,
    /// Каталог файлового кэша медиа.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    /// Максимальный размер одного исходящего файла, байт. Должен быть > 0.
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    /// Верхняя граница файлового кэша медиа, байт. `0` ⇒ кэш не подрезается.
    #[serde(default = "default_max_cache_bytes")]
    pub max_cache_bytes: u64,
    /// Догонять ли пропущенные апдейты после реконнекта.
    #[serde(default = "default_catch_up")]
    pub catch_up: bool,
    /// Глубина очереди апдейтов. Должна быть > 0.
    #[serde(default = "default_update_queue_limit")]
    pub update_queue_limit: usize,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            api_id: None,
            api_hash: None,
            session_path: default_session_path(),
            cache_dir: default_cache_dir(),
            max_upload_bytes: default_max_upload_bytes(),
            max_cache_bytes: default_max_cache_bytes(),
            catch_up: default_catch_up(),
            update_queue_limit: default_update_queue_limit(),
        }
    }
}

impl TelegramConfig {
    /// Полная загрузка: defaults → optional TOML → env-override → validate.
    ///
    /// Никогда не паникует и не возвращает ошибку: невалидные значения
    /// чинятся дефолтами с warn-логом (см. [`Self::validate_and_fix`]).
    pub fn load() -> Self {
        let mut cfg = Self::load_from_file(&config_path());
        cfg.apply_env();
        cfg.validate_and_fix();
        cfg
    }

    /// Читает конфиг из TOML-файла. Файла нет — дефолты (это штатный путь:
    /// у большинства пользователей `telegram.toml` не существует, креды
    /// вводятся в UI). Файл битый — warn-лог и дефолты.
    pub fn load_from_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(body) => match toml::from_str::<TelegramConfig>(&body) {
                Ok(c) => {
                    tracing::info!(target: "forge_telegram", path = %path.display(), "forge-telegram: loaded config from TOML");
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        target: "forge_telegram",
                        path = %path.display(),
                        error = %e,
                        "forge-telegram: failed to parse telegram.toml; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    target: "forge_telegram",
                    path = %path.display(),
                    "forge-telegram: telegram.toml not found, using defaults"
                );
                Self::default()
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_telegram",
                    path = %path.display(),
                    error = %e,
                    "forge-telegram: failed to read telegram.toml; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Применяет env-override `FORGE_TELEGRAM_*`. Пустые строки и мусор
    /// (нечисловой `API_ID`) игнорируются с warn-логом — env не должен уметь
    /// «сломать» конфиг.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_API_ID") {
            if !v.is_empty() {
                match v.parse::<i32>() {
                    Ok(n) => self.api_id = Some(n),
                    Err(e) => tracing::warn!(
                        target: "forge_telegram",
                        value = %v,
                        error = %e,
                        "forge-telegram: FORGE_TELEGRAM_API_ID is not an i32; ignored"
                    ),
                }
            }
        }
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_API_HASH") {
            if !v.is_empty() {
                self.api_hash = Some(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_SESSION_PATH") {
            if !v.is_empty() {
                self.session_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_CACHE_DIR") {
            if !v.is_empty() {
                self.cache_dir = PathBuf::from(v);
            }
        }
        apply_env_u64(
            "FORGE_TELEGRAM_MAX_UPLOAD_BYTES",
            &mut self.max_upload_bytes,
        );
        apply_env_u64("FORGE_TELEGRAM_MAX_CACHE_BYTES", &mut self.max_cache_bytes);
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_CATCH_UP") {
            if let Some(b) = parse_truthy(&v) {
                self.catch_up = b;
            } else if !v.is_empty() {
                tracing::warn!(
                    target: "forge_telegram",
                    value = %v,
                    "forge-telegram: FORGE_TELEGRAM_CATCH_UP is not a boolean; ignored"
                );
            }
        }
        if let Ok(v) = std::env::var("FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT") {
            if !v.is_empty() {
                match v.parse::<usize>() {
                    Ok(n) => self.update_queue_limit = n,
                    Err(e) => tracing::warn!(
                        target: "forge_telegram",
                        value = %v,
                        error = %e,
                        "forge-telegram: FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT is not a usize; ignored"
                    ),
                }
            }
        }
    }

    /// Проверяет инварианты конфига, НЕ меняя его.
    ///
    /// # Errors
    ///
    /// - `api_hash` задан, но пуст/из одних пробелов;
    /// - `max_upload_bytes == 0` (иначе ни один файл не отправить);
    /// - `update_queue_limit == 0` (иначе апдейты некуда складывать).
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(hash) = &self.api_hash {
            if hash.trim().is_empty() {
                anyhow::bail!("api_hash is set but empty");
            }
        }
        if self.max_upload_bytes == 0 {
            anyhow::bail!("max_upload_bytes must be > 0");
        }
        if self.update_queue_limit == 0 {
            anyhow::bail!("update_queue_limit must be > 0");
        }
        Ok(())
    }

    /// Чинит невалидные значения дефолтами (с warn-логом) и создаёт
    /// родительские каталоги для `session_path` / `cache_dir`.
    ///
    /// Используется в [`Self::load`]: конфиг пользователя не должен ронять
    /// старт сервера. Ошибка создания каталогов не фатальна — она всплывёт
    /// позже, при реальной записи сессии/кэша, с более точным контекстом.
    pub fn validate_and_fix(&mut self) {
        if let Some(hash) = &self.api_hash {
            if hash.trim().is_empty() {
                tracing::warn!(target: "forge_telegram", "forge-telegram: empty api_hash; treating as not configured");
                self.api_hash = None;
            }
        }
        if self.max_upload_bytes == 0 {
            tracing::warn!(
                target: "forge_telegram",
                "forge-telegram: max_upload_bytes must be > 0; falling back to default"
            );
            self.max_upload_bytes = default_max_upload_bytes();
        }
        if self.update_queue_limit == 0 {
            tracing::warn!(
                target: "forge_telegram",
                "forge-telegram: update_queue_limit must be > 0; falling back to default"
            );
            self.update_queue_limit = default_update_queue_limit();
        }
        if let Err(e) = self.ensure_dirs() {
            tracing::warn!(
                target: "forge_telegram",
                error = %e,
                "forge-telegram: failed to create config directories"
            );
        }
    }

    /// Создаёт родительский каталог `session_path` и сам `cache_dir`.
    ///
    /// # Errors
    ///
    /// Пробрасывает io-ошибку `create_dir_all` с путём в контексте.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.session_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("failed to create {}: {e}", parent.display())
                })?;
            }
        }
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", self.cache_dir.display()))?;
        Ok(())
    }

    /// Есть ли у плагина оба креда приложения (api_id + непустой api_hash).
    ///
    /// Из этого выводится начальное состояние логина: `true` ⇒
    /// [`crate::login::LoginFlow::Disconnected`], `false` ⇒ `NotConfigured`.
    pub fn has_credentials(&self) -> bool {
        self.api_id.is_some()
            && self
                .api_hash
                .as_ref()
                .is_some_and(|h| !h.trim().is_empty())
    }

    /// Сохраняет введённые пользователем креды в TOML-конфиг по пути
    /// [`config_path`] (обычно `~/.config/forge/telegram.toml`; env-override
    /// `FORGE_TELEGRAM_CONFIG_PATH` уважается — тот же файл, что читает
    /// [`Self::load`]).
    ///
    /// См. [`Self::save_credentials_to`] — здесь только подстановка
    /// пути конфига.
    ///
    /// # Errors
    ///
    /// Любая ошибка записи; целевой файл при этом остаётся нетронутым.
    pub fn save_credentials(&mut self, api_id: i32, api_hash: &str) -> anyhow::Result<()> {
        self.save_credentials_to(&config_path(), api_id, api_hash)
    }

    /// Атомарно записывает `api_id`/`api_hash` в TOML-конфиг по пути `path`
    /// и обновляет их в памяти.
    ///
    /// Инварианты (ради них функция и существует отдельно от `serde`):
    ///
    /// 1. **read-modify-write** — остальные поля пользовательского
    ///    `telegram.toml` (`session_path`, лимиты, `catch_up` и любые
    ///    незнакомые ключи) сохраняются как есть;
    /// 2. **права `0600` выставляются в момент создания файла** — секрет
    ///    ни на мгновение не лежит на диске с более широкими правами;
    /// 3. **атомарность** — содержимое пишется во временный файл в том же
    ///    каталоге, `fsync`, затем `rename` поверх целевого: читатель
    ///    видит либо старый файл, либо новый, но никогда обрезанный;
    /// 4. при любой ошибке временный файл удаляется, целевой не меняется.
    ///
    /// `api_hash` НИКОГДА не попадает в логи — пишется только `api_id`.
    ///
    /// # Errors
    ///
    /// Пустой `api_hash`; невозможность создать каталог/файл; ошибка
    /// записи, `fsync` или `rename`.
    pub fn save_credentials_to(
        &mut self,
        path: &Path,
        api_id: i32,
        api_hash: &str,
    ) -> anyhow::Result<()> {
        let api_hash = api_hash.trim();
        if api_hash.is_empty() {
            anyhow::bail!("api_hash must not be empty");
        }

        // 1. read-modify-write: не затираем пользовательские поля.
        let mut doc: toml::Table = match std::fs::read_to_string(path) {
            Ok(body) => toml::from_str(&body).unwrap_or_else(|e| {
                tracing::warn!(
                    target: "forge_telegram",
                    path = %path.display(),
                    error = %e,
                    "forge-telegram: telegram.toml is not valid TOML; rewriting it from scratch"
                );
                toml::Table::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(anyhow::anyhow!("failed to read {}: {e}", path.display())),
        };
        doc.insert("api_id".into(), toml::Value::Integer(i64::from(api_id)));
        doc.insert("api_hash".into(), toml::Value::String(api_hash.to_string()));
        let body = toml::to_string_pretty(&doc)
            .map_err(|e| anyhow::anyhow!("failed to serialize telegram.toml: {e}"))?;

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
        }

        // 2-3. tmp в том же каталоге (rename атомарен только внутри одной
        //      ФС), права 0600 при создании, fsync, rename.
        let tmp = tmp_path_for(path);
        let outcome = write_secret_file(&tmp, body.as_bytes())
            .and_then(|()| {
                std::fs::rename(&tmp, path).map_err(|e| {
                    anyhow::anyhow!(
                        "failed to move {} onto {}: {e}",
                        tmp.display(),
                        path.display()
                    )
                })
            })
            .and_then(|()| set_owner_only(path));

        if let Err(e) = outcome {
            // 4. Никаких частичных состояний: убираем tmp, целевой файл
            //    остался прежним.
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        self.api_id = Some(api_id);
        self.api_hash = Some(api_hash.to_string());
        tracing::info!(
            target: "forge_telegram",
            path = %path.display(),
            api_id,
            "forge-telegram: saved application credentials (api_hash is never logged)"
        );
        Ok(())
    }
}

/// Путь временного файла рядом с целевым: `.<имя>.<pid>.tmp`.
///
/// PID в имени защищает от коллизии двух процессов, пишущих конфиг
/// одновременно: последний `rename` выигрывает — это ожидаемое поведение,
/// но обрезанного файла не увидит никто.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "telegram.toml".to_string());
    let tmp_name = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// Создаёт файл с правами `0600` ДО записи содержимого, пишет и `fsync`'ит.
pub(crate) fn write_secret_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", path.display()))?;
    // Файл мог остаться с прошлого раза (create, а не create_new) — тогда
    // mode() не применяется, поэтому права выставляются явно, и всё это
    // ДО первого write_all.
    set_owner_only(path)?;
    f.write_all(bytes)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| anyhow::anyhow!("failed to fsync {}: {e}", path.display()))?;
    Ok(())
}

/// Выставляет права `0600` (на не-Unix — no-op).
pub(crate) fn set_owner_only(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("failed to chmod 600 {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Парсит u64 из env в `target`; мусор — warn + пропуск.
fn apply_env_u64(key: &str, target: &mut u64) {
    if let Ok(v) = std::env::var(key) {
        if v.is_empty() {
            return;
        }
        match v.parse::<u64>() {
            Ok(n) => *target = n,
            Err(e) => tracing::warn!(
                target: "forge_telegram",
                key,
                value = %v,
                error = %e,
                "forge-telegram: env value is not a u64; ignored"
            ),
        }
    }
}

/// `1/true/yes/on` → `Some(true)`, `0/false/no/off` → `Some(false)`,
/// остальное → `None`. Регистр не важен.
fn parse_truthy(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Все env-тесты сериализуются между собой: `std::env` — process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const ENV_KEYS: &[&str] = &[
        "FORGE_TELEGRAM_CONFIG_PATH",
        "FORGE_TELEGRAM_API_ID",
        "FORGE_TELEGRAM_API_HASH",
        "FORGE_TELEGRAM_SESSION_PATH",
        "FORGE_TELEGRAM_CACHE_DIR",
        "FORGE_TELEGRAM_MAX_UPLOAD_BYTES",
        "FORGE_TELEGRAM_MAX_CACHE_BYTES",
        "FORGE_TELEGRAM_CATCH_UP",
        "FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT",
    ];

    fn clear_env() {
        for k in ENV_KEYS {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn defaults_without_file_and_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope/telegram.toml");

        let mut cfg = TelegramConfig::load_from_file(&missing);
        cfg.apply_env();

        assert_eq!(cfg, TelegramConfig::default());
        assert!(cfg.api_id.is_none());
        assert!(cfg.api_hash.is_none());
        assert!(!cfg.has_credentials());
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.max_cache_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert_eq!(cfg.update_queue_limit, DEFAULT_UPDATE_QUEUE_LIMIT);
        assert!(cfg.catch_up);
        clear_env();
    }

    #[test]
    fn toml_file_overrides_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "api_id = 12345\napi_hash = \"deadbeef\"\nupdate_queue_limit = 32\ncatch_up = false\n",
        )
        .unwrap();

        let cfg = TelegramConfig::load_from_file(&path);
        assert_eq!(cfg.api_id, Some(12345));
        assert_eq!(cfg.api_hash.as_deref(), Some("deadbeef"));
        assert_eq!(cfg.update_queue_limit, 32);
        assert!(!cfg.catch_up);
        // Незаданные поля — дефолты.
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert!(cfg.has_credentials());
        clear_env();
    }

    #[test]
    fn env_overrides_toml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "api_id = 111\napi_hash = \"from_toml\"\nupdate_queue_limit = 32\n",
        )
        .unwrap();

        std::env::set_var("FORGE_TELEGRAM_API_ID", "222");
        std::env::set_var("FORGE_TELEGRAM_API_HASH", "from_env");
        std::env::set_var("FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT", "64");
        std::env::set_var("FORGE_TELEGRAM_CATCH_UP", "off");
        std::env::set_var("FORGE_TELEGRAM_MAX_UPLOAD_BYTES", "1024");

        let mut cfg = TelegramConfig::load_from_file(&path);
        cfg.apply_env();

        assert_eq!(cfg.api_id, Some(222), "env перекрывает TOML");
        assert_eq!(cfg.api_hash.as_deref(), Some("from_env"));
        assert_eq!(cfg.update_queue_limit, 64);
        assert!(!cfg.catch_up);
        assert_eq!(cfg.max_upload_bytes, 1024);
        clear_env();
    }

    #[test]
    fn env_garbage_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("FORGE_TELEGRAM_API_ID", "not-a-number");
        std::env::set_var("FORGE_TELEGRAM_UPDATE_QUEUE_LIMIT", "-5");
        std::env::set_var("FORGE_TELEGRAM_CATCH_UP", "maybe");

        let mut cfg = TelegramConfig::default();
        cfg.apply_env();

        assert!(cfg.api_id.is_none(), "мусорный api_id игнорируется");
        assert_eq!(cfg.update_queue_limit, DEFAULT_UPDATE_QUEUE_LIMIT);
        assert!(cfg.catch_up);
        clear_env();
    }

    #[test]
    fn validate_rejects_zero_update_queue_limit() {
        let cfg = TelegramConfig {
            update_queue_limit: 0,
            ..TelegramConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("update_queue_limit"), "got: {err}");
    }

    #[test]
    fn validate_rejects_zero_upload_limit_and_empty_hash() {
        let cfg = TelegramConfig {
            max_upload_bytes: 0,
            ..TelegramConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = TelegramConfig {
            api_hash: Some("   ".into()),
            ..TelegramConfig::default()
        };
        assert!(cfg.validate().is_err());

        assert!(TelegramConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_and_fix_repairs_invalid_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = TelegramConfig {
            api_hash: Some("".into()),
            max_upload_bytes: 0,
            update_queue_limit: 0,
            session_path: dir.path().join("nested/telegram.session"),
            cache_dir: dir.path().join("nested/cache"),
            ..TelegramConfig::default()
        };
        cfg.validate_and_fix();

        assert!(cfg.api_hash.is_none());
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.update_queue_limit, DEFAULT_UPDATE_QUEUE_LIMIT);
        assert!(cfg.validate().is_ok());
        assert!(dir.path().join("nested").is_dir(), "parent сессии создан");
        assert!(cfg.cache_dir.is_dir(), "cache_dir создан");
    }

    #[test]
    fn load_does_not_panic_without_file_and_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        // Полная изоляция от реального ~/.config/forge: конфиг, сессия и кэш
        // указывают в tempdir (файла конфига там нет — это штатный путь).
        std::env::set_var(
            "FORGE_TELEGRAM_CONFIG_PATH",
            dir.path().join("telegram.toml"),
        );
        std::env::set_var("FORGE_TELEGRAM_SESSION_PATH", dir.path().join("s.session"));
        std::env::set_var("FORGE_TELEGRAM_CACHE_DIR", dir.path().join("cache"));

        let cfg = TelegramConfig::load();
        assert!(cfg.validate().is_ok());
        assert!(
            !cfg.has_credentials(),
            "load() не должен подтянуть креды с реальной машины"
        );
        clear_env();
    }

    /// `FORGE_TELEGRAM_CONFIG_PATH` переопределяет и чтение ([`TelegramConfig::load`]),
    /// и запись ([`TelegramConfig::save_credentials`]) — всегда один файл.
    #[test]
    fn config_path_env_overrides_load_and_save() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("nested/telegram.toml");
        std::env::set_var("FORGE_TELEGRAM_CONFIG_PATH", &toml_path);
        std::env::set_var("FORGE_TELEGRAM_SESSION_PATH", dir.path().join("s.session"));
        std::env::set_var("FORGE_TELEGRAM_CACHE_DIR", dir.path().join("cache"));

        assert_eq!(config_path(), toml_path);

        let mut cfg = TelegramConfig::load();
        assert!(!cfg.has_credentials(), "файла ещё нет — кредов нет");

        cfg.save_credentials(777, "hash777").unwrap();
        assert!(toml_path.is_file(), "save пишет по env-пути, не в $HOME");

        let reloaded = TelegramConfig::load();
        assert_eq!(reloaded.api_id, Some(777));
        assert_eq!(reloaded.api_hash.as_deref(), Some("hash777"));
        clear_env();
    }

    #[test]
    fn save_credentials_writes_atomically_with_owner_only_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/telegram.toml");

        let mut cfg = TelegramConfig::default();
        cfg.save_credentials_to(&path, 12345, "  deadbeef  ").unwrap();

        // В памяти креды появились (с обрезанными пробелами).
        assert_eq!(cfg.api_id, Some(12345));
        assert_eq!(cfg.api_hash.as_deref(), Some("deadbeef"));
        assert!(cfg.has_credentials());

        // На диске лежит валидный TOML с теми же значениями.
        let reloaded = TelegramConfig::load_from_file(&path);
        assert_eq!(reloaded.api_id, Some(12345));
        assert_eq!(reloaded.api_hash.as_deref(), Some("deadbeef"));

        // Права ровно 0600 — api_hash это секрет приложения.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "ожидались права -rw-------, получили {mode:o}");
        }

        // Временных файлов не осталось.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "остались tmp-файлы: {leftovers:?}");
    }

    #[test]
    fn save_credentials_preserves_other_user_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "update_queue_limit = 32\ncatch_up = false\napi_id = 1\napi_hash = \"old\"\n\
             some_future_key = \"keep me\"\n",
        )
        .unwrap();

        let mut cfg = TelegramConfig::default();
        cfg.save_credentials_to(&path, 999, "newhash").unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Table = toml::from_str(&body).unwrap();
        assert_eq!(doc["api_id"].as_integer(), Some(999));
        assert_eq!(doc["api_hash"].as_str(), Some("newhash"));
        assert_eq!(doc["update_queue_limit"].as_integer(), Some(32));
        assert_eq!(doc["catch_up"].as_bool(), Some(false));
        assert_eq!(
            doc["some_future_key"].as_str(),
            Some("keep me"),
            "незнакомые ключи пользователя не должны исчезать"
        );
    }

    #[test]
    fn save_credentials_rejects_empty_hash_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");

        let mut cfg = TelegramConfig::default();
        assert!(cfg.save_credentials_to(&path, 1, "   ").is_err());
        assert!(cfg.api_id.is_none(), "память не изменилась");
        assert!(!path.exists(), "файл не создан");
    }

    #[test]
    fn save_credentials_overwrites_and_keeps_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");

        let mut cfg = TelegramConfig::default();
        cfg.save_credentials_to(&path, 1, "first").unwrap();
        cfg.save_credentials_to(&path, 2, "second").unwrap();

        let reloaded = TelegramConfig::load_from_file(&path);
        assert_eq!(reloaded.api_id, Some(2));
        assert_eq!(reloaded.api_hash.as_deref(), Some("second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn parse_truthy_matrix() {
        assert_eq!(parse_truthy("1"), Some(true));
        assert_eq!(parse_truthy("TRUE"), Some(true));
        assert_eq!(parse_truthy("on"), Some(true));
        assert_eq!(parse_truthy("0"), Some(false));
        assert_eq!(parse_truthy("Off"), Some(false));
        assert_eq!(parse_truthy("nope"), None);
    }

    // ------------------------------------------------------------------
    // Краевые случаи: переполнения в env, битый TOML, ФС-ошибки, HOME.
    // ------------------------------------------------------------------

    /// Числа, не влезающие в целевой тип (u64::MAX+1, отрицательные,
    /// i32::MAX+1 для api_id) — реальный мусор из shell-скриптов. Они
    /// обязаны игнорироваться, а не портить значения и не паниковать.
    #[test]
    fn env_numeric_overflow_and_negative_are_ignored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("FORGE_TELEGRAM_MAX_UPLOAD_BYTES", "18446744073709551616");
        std::env::set_var("FORGE_TELEGRAM_MAX_CACHE_BYTES", "-1");
        std::env::set_var("FORGE_TELEGRAM_API_ID", "2147483648"); // i32::MAX + 1

        let mut cfg = TelegramConfig::default();
        cfg.apply_env();

        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.max_cache_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert!(cfg.api_id.is_none(), "переполненный api_id игнорируется");
        clear_env();
    }

    /// `export FORGE_TELEGRAM_FOO=` (пустая строка) — частый артефакт
    /// shell-конфигов. Пустой env не должен затирать значения из TOML.
    #[test]
    fn env_empty_values_do_not_override_toml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "api_id = 7\napi_hash = \"keep\"\nsession_path = \"/tmp/custom.session\"\nmax_upload_bytes = 42\n",
        )
        .unwrap();
        for k in ENV_KEYS {
            std::env::set_var(k, "");
        }

        let mut cfg = TelegramConfig::load_from_file(&path);
        cfg.apply_env();

        assert_eq!(cfg.api_id, Some(7));
        assert_eq!(cfg.api_hash.as_deref(), Some("keep"));
        assert_eq!(cfg.session_path, PathBuf::from("/tmp/custom.session"));
        assert_eq!(cfg.max_upload_bytes, 42);
        clear_env();
    }

    /// `export FORGE_TELEGRAM_API_HASH="   "` проходит apply_env (строка
    /// непустая), но validate_and_fix обязан свести её к None — иначе
    /// has_credentials() дал бы false, а api_hash остался бы «задан».
    #[test]
    fn env_whitespace_api_hash_is_neutralized_by_fix() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("FORGE_TELEGRAM_API_HASH", "   ");
        let dir = tempfile::tempdir().unwrap();

        let mut cfg = TelegramConfig {
            session_path: dir.path().join("s.session"),
            cache_dir: dir.path().join("cache"),
            ..TelegramConfig::default()
        };
        cfg.apply_env();
        assert_eq!(cfg.api_hash.as_deref(), Some("   "), "apply_env строку принял");

        cfg.validate_and_fix();
        assert!(cfg.api_hash.is_none(), "пробельный api_hash = не настроен");
        assert!(!cfg.has_credentials());
        clear_env();
    }

    /// Неверный ТИП одного поля (api_id строкой) отвергает файл ЦЕЛИКОМ:
    /// никаких частичных применений — иначе пользователь получил бы
    /// непредсказуемую смесь своего конфига и дефолтов.
    #[test]
    fn toml_wrong_field_type_falls_back_to_full_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "api_id = \"12345\"\napi_hash = \"validhash\"\nupdate_queue_limit = 8\n",
        )
        .unwrap();

        let cfg = TelegramConfig::load_from_file(&path);
        assert_eq!(cfg, TelegramConfig::default(), "битый файл ⇒ только дефолты");
    }

    /// Обрыв записи на полпути (диск кончился, kill -9): незакрытая
    /// строка. Дефолты, без паники.
    #[test]
    fn toml_truncated_mid_string_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(&path, "api_id = 1\napi_hash = \"dead").unwrap();

        let cfg = TelegramConfig::load_from_file(&path);
        assert_eq!(cfg, TelegramConfig::default());
    }

    /// api_id, не влезающий в i32 (Telegram выдаёт только i32) — файл
    /// отвергается без паники при десериализации числа.
    #[test]
    fn toml_api_id_overflowing_i32_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(&path, "api_id = 3000000000\napi_hash = \"hash\"\n").unwrap();

        let cfg = TelegramConfig::load_from_file(&path);
        assert!(cfg.api_id.is_none());
        assert!(cfg.api_hash.is_none(), "файл отвергнут целиком");
    }

    /// Незнакомые ключи (конфиг от более новой версии devforge) не должны
    /// ронять парсинг — известные поля применяются как обычно.
    #[test]
    fn toml_unknown_keys_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::write(
            &path,
            "api_id = 9\napi_hash = \"h\"\nfuture_feature_flag = true\n[future_section]\nx = 1\n",
        )
        .unwrap();

        let cfg = TelegramConfig::load_from_file(&path);
        assert_eq!(cfg.api_id, Some(9), "новые ключи не мешают старым");
        assert_eq!(cfg.api_hash.as_deref(), Some("h"));
    }

    /// systemd-юнит/контейнер без $HOME: дефолтные пути откатываются на
    /// относительные, а не паникуют на var_os.
    #[test]
    fn default_paths_fall_back_to_relative_without_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("HOME");
        std::env::remove_var("HOME");

        let cfg_path = default_config_path();
        let sess_path = default_session_path();
        let cache = default_cache_dir();

        if let Some(h) = saved_home {
            std::env::set_var("HOME", h);
        }
        assert_eq!(cfg_path, PathBuf::from("./telegram.toml"));
        assert_eq!(sess_path, PathBuf::from("./telegram.session"));
        assert_eq!(cache, PathBuf::from("./telegram-cache"));
    }

    /// Пользователь по ошибке создал ~/.config/forge/telegram.toml
    /// КАТАЛОГОМ: read_to_string даёт не-NotFound ошибку — warn + дефолты.
    #[test]
    fn load_from_file_on_directory_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig::load_from_file(dir.path());
        assert_eq!(cfg, TelegramConfig::default());
    }

    /// Целевой путь занят каталогом — save обязан вернуть ошибку, не
    /// изменив ни память, ни ФС (каталог остаётся на месте).
    #[test]
    fn save_credentials_to_fails_when_path_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        std::fs::create_dir(&path).unwrap();

        let mut cfg = TelegramConfig::default();
        assert!(cfg.save_credentials_to(&path, 5, "hash").is_err());
        assert!(cfg.api_id.is_none(), "память не изменилась при ошибке");
        assert!(path.is_dir(), "каталог не тронут");
    }

    /// Каталог конфига без права записи (r-x): ошибка наружу, целевой файл
    /// не появился, tmp-мусор не остался.
    #[cfg(unix)]
    #[test]
    fn save_credentials_to_read_only_dir_fails_without_leftovers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("cfg");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = parent.join("telegram.toml");

        let mut cfg = TelegramConfig::default();
        let res = cfg.save_credentials_to(&path, 5, "hash");

        // Возвращаем права до assert'ов, чтобы TempDir убрался при панике.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(res.is_err());
        assert!(cfg.api_id.is_none() && cfg.api_hash.is_none());
        assert!(!path.exists(), "целевой файл не должен появиться");
        let leftovers: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(leftovers.is_empty(), "остался мусор: {leftovers:?}");
    }

    /// Сценарий после падения процесса: от прошлого запуска остался tmp-файл
    /// с широкими правами. `write_secret_file` открывает его через create
    /// (не create_new), поэтому обязан явно ужать права до 0600 ДО записи
    /// секрета — иначе api_hash полежал бы world-readable.
    #[cfg(unix)]
    #[test]
    fn save_credentials_tightens_perms_of_stale_tmp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.toml");
        let tmp = tmp_path_for(&path);
        std::fs::write(&tmp, b"stale garbage from a crashed process").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();

        let mut cfg = TelegramConfig::default();
        cfg.save_credentials_to(&path, 77, "secret").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "унаследованные 0666 обязаны ужаться");
        let reloaded = TelegramConfig::load_from_file(&path);
        assert_eq!(reloaded.api_id, Some(77));
        assert_eq!(reloaded.api_hash.as_deref(), Some("secret"));
        assert!(!tmp.exists(), "tmp переименован, мусора нет");
    }

    /// Минимально валидные значения (1 байт, очередь глубиной 1, непустой
    /// hash) validate_and_fix трогать НЕ должен — ловит регресс `>=` vs `>`.
    #[test]
    fn validate_and_fix_keeps_minimal_valid_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = TelegramConfig {
            api_hash: Some("x".into()),
            max_upload_bytes: 1,
            max_cache_bytes: 0, // 0 для кэша ВАЛИДЕН («не подрезать»)
            update_queue_limit: 1,
            session_path: dir.path().join("s.session"),
            cache_dir: dir.path().join("cache"),
            ..TelegramConfig::default()
        };
        cfg.validate_and_fix();

        assert_eq!(cfg.api_hash.as_deref(), Some("x"));
        assert_eq!(cfg.max_upload_bytes, 1);
        assert_eq!(cfg.max_cache_bytes, 0);
        assert_eq!(cfg.update_queue_limit, 1);
        assert!(cfg.validate().is_ok());
    }
}
