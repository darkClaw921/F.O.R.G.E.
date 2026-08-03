//! Конфигурация плагина VK и хранилище единственного секрета — токена.
//!
//! Цепочка приоритетов повторяет эталон Echo/Telegram:
//!
//! 1. [`VkConfig::default`] — статические дефолты.
//! 2. [`VkConfig::load_from_file`] — `~/.config/forge/vk.toml` (путь
//!    переопределяется `FORGE_VK_CONFIG_PATH`, см. [`config_path`]), если файл
//!    есть; отсутствие файла — НЕ ошибка, битый TOML — warn + дефолты.
//! 3. [`VkConfig::apply_env`] — env-override `FORGE_VK_*`:
//!    - `FORGE_VK_ACCESS_TOKEN`
//!    - `FORGE_VK_API_VERSION`
//!    - `FORGE_VK_BASE_URL`
//!    - `FORGE_VK_CACHE_DIR`
//!    - `FORGE_VK_MAX_UPLOAD_BYTES`
//!    - `FORGE_VK_MAX_CACHE_BYTES`
//!    - `FORGE_VK_RATE_LIMIT_RPS`
//!    - `FORGE_VK_LONGPOLL_WAIT_SECS`
//! 4. [`VkConfig::validate`] / [`VkConfig::validate_and_fix`] — проверка
//!    инвариантов, зажим числовых полей в разумные границы и создание
//!    `cache_dir`.
//!
//! [`VkConfig::load`] делает всё вместе и НИКОГДА не паникует: любая проблема
//! (нет файла, битый TOML, мусор в env, невалидное значение) — это warn-лог и
//! откат на дефолт.
//!
//! ## Почему токен — newtype без `Serialize`
//!
//! У Telegram защита `api_hash` держалась на дисциплине и одном тесте
//! (`status_never_leaks_api_hash`). Здесь токен — ЕДИНСТВЕННЫЙ секрет и
//! ЕДИНСТВЕННАЯ учётка, поэтому дешевле сделать утечку ошибкой компиляции:
//! [`SecretToken`] не реализует `Serialize` вообще, а значит DTO с токеном
//! физически не соберётся. Для `vk.toml` сериализация делается вручную
//! (`toml::Value::String(t.expose().into())`) — ровно одна строка, ровно одно
//! место ([`VkConfig::save_token_to`]).
//!
//! `Deserialize` при этом реализован: направление «внутрь» безопасно, и без
//! него `vk.toml` пришлось бы разбирать руками.
//!
//! ## Что добавлено против Telegram
//!
//! При загрузке файл с правами шире `0600` **чинится** (`chmod 600`) с
//! warn-логом. Токен пользователь вставляет руками и вполне может создать
//! `vk.toml` сам через `echo >` (то есть с umask'овыми `0644`); у Telegram
//! такого риска не было — `api_hash` всегда писали мы.
//!
//! ## Почему `base_url` обязателен
//!
//! Это единственный способ протестировать транспорт, LongPoll и upload без
//! живого токена: `plugins/vk/tests/vk_stub.rs` (Ф2) поднимает локальный
//! axum-стенд и подсовывает его адрес сюда. Побочная выгода — переезд VK на
//! `api.vk.ru` меняет одну строку в `vk.toml`, без пересборки.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Версия API VK по умолчанию.
pub const DEFAULT_API_VERSION: &str = "5.199";

/// Базовый URL методов API. Токен выдаёт `oauth.vk.ru`, а вот хост методов
/// живёт здесь — при переезде на `api.vk.ru` меняется одна строка конфига.
pub const DEFAULT_BASE_URL: &str = "https://api.vk.com/method";

/// Максимальный размер одного исходящего файла — 2 GiB.
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Верхняя граница файлового кэша медиа — 512 MiB. LRU-sweeper (Ф7)
/// подчищает по mtime до этого значения.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Лимит запросов к API для ПОЛЬЗОВАТЕЛЬСКОГО токена — 3 запроса в секунду.
/// Значение известно заранее (в отличие от телеграмного `FLOOD_WAIT`,
/// который узнаётся по факту), поэтому троттлер соблюдает его превентивно.
pub const DEFAULT_RATE_LIMIT_RPS: u32 = 3;

/// Сколько LongPoll-сервер держит соединение открытым, секунды.
pub const DEFAULT_LONGPOLL_WAIT_SECS: u64 = 25;

/// Верхняя граница `rate_limit_rps`. Больше 20 rps пользовательский токен не
/// выдержит ни при каких условиях — значение выше означает опечатку, а не
/// намерение, и зажимается [`VkConfig::validate_and_fix`].
pub const MAX_RATE_LIMIT_RPS: u32 = 20;

/// Верхняя граница `longpoll_wait_secs`: у VK ожидание больше 90 секунд
/// обрывается прокси-серверами, а HTTP-таймаут воркера считается как
/// `wait + 10`.
pub const MAX_LONGPOLL_WAIT_SECS: u64 = 90;

/// Дефолтный путь TOML-конфига плагина (`~/.config/forge/vk.toml`).
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/vk.toml")
    } else {
        PathBuf::from("./vk.toml")
    }
}

/// Фактический путь TOML-конфига: непустой `FORGE_VK_CONFIG_PATH` побеждает,
/// иначе — [`default_config_path`].
///
/// Env-override нужен контейнерам/systemd-юнитам с нестандартным layout'ом и
/// герметичным тестам: без него [`VkConfig::load`] всегда читал бы реальный
/// `~/.config/forge/vk.toml` машины разработчика, а [`VkConfig::save_token`]
/// писал бы туда же — то есть прогон `cargo test` затирал бы живой токен.
pub fn config_path() -> PathBuf {
    match std::env::var_os("FORGE_VK_CONFIG_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_config_path(),
    }
}

/// Дефолтный каталог файлового кэша медиа (`~/.forge/vk-cache`).
pub fn default_cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge/vk-cache")
    } else {
        PathBuf::from("./vk-cache")
    }
}

fn default_api_version() -> String {
    DEFAULT_API_VERSION.to_string()
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_string()
}

fn default_max_upload_bytes() -> u64 {
    DEFAULT_MAX_UPLOAD_BYTES
}

fn default_max_cache_bytes() -> u64 {
    DEFAULT_MAX_CACHE_BYTES
}

fn default_rate_limit_rps() -> u32 {
    DEFAULT_RATE_LIMIT_RPS
}

fn default_longpoll_wait_secs() -> u64 {
    DEFAULT_LONGPOLL_WAIT_SECS
}

/// Пользовательский access-токен VK.
///
/// Newtype существует ради ОДНОГО инварианта: `Serialize` для него не
/// реализован и реализован не будет. Любая попытка положить токен в DTO,
/// WS-кадр или лог-структуру перестаёт компилироваться — это дешевле, чем
/// дисциплина и тест на каждое новое поле.
///
/// Читать содержимое можно только через [`SecretToken::expose`] — grep по
/// `expose()` показывает ВСЕ места, где секрет покидает newtype.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretToken(String);

impl SecretToken {
    /// Оборачивает строку, обрезая пробелы по краям (пользователь вставляет
    /// токен из буфера, где легко захватить перенос строки).
    pub fn new(raw: impl AsRef<str>) -> Self {
        Self(raw.as_ref().trim().to_string())
    }

    /// ЕДИНСТВЕННАЯ точка чтения секрета.
    ///
    /// Намеренно называется «expose», а не `as_str`: вызов должен выглядеть
    /// подозрительно в code review.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Пуст ли токен (то есть плагин не настроен).
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Отпечаток для ЛОГОВ: `vk1.a…4f9c3f`.
    ///
    /// Нужен ровно для одного вопроса — «тот ли токен вставлен» — когда у
    /// пользователя их три в блокноте. В HTTP-ответы (`GET /api/vk/status`)
    /// отпечаток НЕ кладётся: любая часть секрета в ответе — это часть
    /// секрета (см. `routes::StatusDto`).
    ///
    /// Короткие строки не раскрываются вовсе: у 8-символьного «токена»
    /// «начало + хвост» — это он сам целиком.
    pub fn fingerprint(&self) -> String {
        let t = self.0.trim();
        if t.is_empty() {
            return "<none>".to_string();
        }
        let chars: Vec<char> = t.chars().collect();
        if chars.len() < 16 {
            return "<short>".to_string();
        }
        let head: String = chars[..5].iter().collect();
        let tail: String = chars[chars.len() - 6..].iter().collect();
        format!("{head}…{tail}")
    }
}

/// `Debug` НИКОГДА не печатает содержимое: `{:?}` на конфиге, на состоянии
/// или в `tracing::debug!` не должен раскрывать токен ни при каких условиях.
impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<&str> for SecretToken {
    fn from(v: &str) -> Self {
        Self::new(v)
    }
}

impl From<String> for SecretToken {
    fn from(v: String) -> Self {
        Self::new(v)
    }
}

/// Полная конфигурация плагина VK.
///
/// `Serialize` НЕ выводится намеренно (см. шапку модуля): структура содержит
/// [`SecretToken`], и производный `Serialize` был бы дырой в инварианте.
/// Запись на диск делает [`VkConfig::save_token_to`] через `toml::Table`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VkConfig {
    /// Пользовательский access-токен. Пустой ⇒ плагин в состоянии
    /// `not_configured`, логин недоступен, но сервер поднимается.
    #[serde(default)]
    pub access_token: SecretToken,
    /// Версия API VK (`v=` в каждом запросе).
    #[serde(default = "default_api_version")]
    pub api_version: String,
    /// Базовый URL методов API. ОБЯЗАТЕЛЕН — см. шапку модуля.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Каталог файлового кэша медиа и курсора LongPoll.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    /// Максимальный размер одного исходящего файла, байт. Должен быть > 0.
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    /// Верхняя граница файлового кэша медиа, байт. `0` ⇒ кэш не подрезается.
    #[serde(default = "default_max_cache_bytes")]
    pub max_cache_bytes: u64,
    /// Лимит запросов к API в секунду (token bucket, Ф2).
    #[serde(default = "default_rate_limit_rps")]
    pub rate_limit_rps: u32,
    /// Сколько секунд LongPoll держит соединение (`wait=` в `a_check`).
    #[serde(default = "default_longpoll_wait_secs")]
    pub longpoll_wait_secs: u64,
    /// Куда писать токен при логине и откуда стирать при логауте.
    ///
    /// Из TOML НЕ читается (`serde(skip)`): путь к файлу внутри самого файла
    /// сбивал бы с толку и позволял бы конфигу «переехать» на чужой. Значение
    /// берётся из [`config_path`], то есть из `FORGE_VK_CONFIG_PATH` либо из
    /// `$HOME`.
    ///
    /// Отдельным ПОЛЕМ (а не вызовом [`config_path`] в момент записи) оно
    /// стало ради тестов логина: `std::env` — process-global, и тест,
    /// подменяющий переменную окружения, гонялся бы с соседним, который в этот
    /// момент пишет токен. С полем каждый тест указывает свой tempdir и не
    /// может задеть реальный `~/.config/forge/vk.toml` пользователя.
    #[serde(skip, default = "config_path")]
    pub config_path: PathBuf,
}

impl Default for VkConfig {
    fn default() -> Self {
        Self {
            access_token: SecretToken::default(),
            api_version: default_api_version(),
            base_url: default_base_url(),
            cache_dir: default_cache_dir(),
            max_upload_bytes: default_max_upload_bytes(),
            max_cache_bytes: default_max_cache_bytes(),
            rate_limit_rps: default_rate_limit_rps(),
            longpoll_wait_secs: default_longpoll_wait_secs(),
            config_path: config_path(),
        }
    }
}

impl VkConfig {
    /// Полная загрузка: defaults → optional TOML → env-override → validate.
    ///
    /// Никогда не паникует и не возвращает ошибку: невалидные значения
    /// чинятся дефолтами с warn-логом (см. [`Self::validate_and_fix`]).
    pub fn load() -> Self {
        let path = config_path();
        let mut cfg = Self::load_from_file(&path);
        // Читаем и пишем ОДИН файл: иначе logout стирал бы токен не оттуда,
        // откуда он был прочитан.
        cfg.config_path = path;
        cfg.apply_env();
        cfg.validate_and_fix();
        cfg
    }

    /// Читает конфиг из TOML-файла.
    ///
    /// Файла нет — дефолты (штатный путь: у большинства пользователей
    /// `vk.toml` не существует, токен вставляется в UI). Файл битый — warn и
    /// дефолты.
    ///
    /// ДОБАВЛЕНИЕ ПРОТИВ TELEGRAM: если файл существует и его права шире
    /// `0600`, они чинятся на месте с warn-логом. Файл с токеном пользователь
    /// вполне мог создать сам (`echo access_token=… > vk.toml`) и получить
    /// umask'овые `0644`; молча читать секрет из world-readable файла — значит
    /// оставить его таким навсегда.
    pub fn load_from_file(path: &Path) -> Self {
        tighten_perms_if_needed(path);
        match std::fs::read_to_string(path) {
            Ok(body) => match toml::from_str::<VkConfig>(&body) {
                Ok(c) => {
                    tracing::info!(
                        target: "forge_vk",
                        path = %path.display(),
                        "forge-vk: loaded config from TOML"
                    );
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        target: "forge_vk",
                        path = %path.display(),
                        error = %e,
                        "forge-vk: failed to parse vk.toml; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    target: "forge_vk",
                    path = %path.display(),
                    "forge-vk: vk.toml not found, using defaults"
                );
                Self::default()
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_vk",
                    path = %path.display(),
                    error = %e,
                    "forge-vk: failed to read vk.toml; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Применяет env-override `FORGE_VK_*`.
    ///
    /// Пустые строки и мусор (нечисловой rps) игнорируются с warn-логом — env
    /// не должен уметь «сломать» конфиг. Значение `FORGE_VK_ACCESS_TOKEN` в
    /// лог не попадает никогда, даже как факт длины.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FORGE_VK_ACCESS_TOKEN") {
            if !v.trim().is_empty() {
                self.access_token = SecretToken::new(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_VK_API_VERSION") {
            if !v.trim().is_empty() {
                self.api_version = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("FORGE_VK_BASE_URL") {
            if !v.trim().is_empty() {
                self.base_url = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("FORGE_VK_CACHE_DIR") {
            if !v.is_empty() {
                self.cache_dir = PathBuf::from(v);
            }
        }
        apply_env_u64("FORGE_VK_MAX_UPLOAD_BYTES", &mut self.max_upload_bytes);
        apply_env_u64("FORGE_VK_MAX_CACHE_BYTES", &mut self.max_cache_bytes);
        apply_env_u64("FORGE_VK_LONGPOLL_WAIT_SECS", &mut self.longpoll_wait_secs);
        if let Ok(v) = std::env::var("FORGE_VK_RATE_LIMIT_RPS") {
            if !v.is_empty() {
                match v.parse::<u32>() {
                    Ok(n) => self.rate_limit_rps = n,
                    Err(e) => tracing::warn!(
                        target: "forge_vk",
                        value = %v,
                        error = %e,
                        "forge-vk: FORGE_VK_RATE_LIMIT_RPS is not a u32; ignored"
                    ),
                }
            }
        }
    }

    /// Проверяет инварианты конфига, НЕ меняя его.
    ///
    /// # Errors
    ///
    /// - пустой `api_version` или `base_url` (без них не собрать ни одного
    ///   запроса);
    /// - `max_upload_bytes == 0` (иначе ни один файл не отправить);
    /// - `rate_limit_rps` вне `1..=MAX_RATE_LIMIT_RPS`;
    /// - `longpoll_wait_secs` вне `1..=MAX_LONGPOLL_WAIT_SECS`.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.api_version.trim().is_empty() {
            anyhow::bail!("api_version must not be empty");
        }
        if self.base_url.trim().is_empty() {
            anyhow::bail!("base_url must not be empty");
        }
        if self.max_upload_bytes == 0 {
            anyhow::bail!("max_upload_bytes must be > 0");
        }
        if self.rate_limit_rps == 0 || self.rate_limit_rps > MAX_RATE_LIMIT_RPS {
            anyhow::bail!("rate_limit_rps must be within 1..={MAX_RATE_LIMIT_RPS}");
        }
        if self.longpoll_wait_secs == 0 || self.longpoll_wait_secs > MAX_LONGPOLL_WAIT_SECS {
            anyhow::bail!("longpoll_wait_secs must be within 1..={MAX_LONGPOLL_WAIT_SECS}");
        }
        Ok(())
    }

    /// Чинит невалидные значения дефолтами (с warn-логом), нормализует
    /// `base_url` (убирает хвостовой `/`) и создаёт `cache_dir`.
    ///
    /// Используется в [`Self::load`]: конфиг пользователя не должен ронять
    /// старт сервера. Ошибка создания каталога не фатальна — она всплывёт
    /// позже, при реальной записи кэша, с более точным контекстом.
    pub fn validate_and_fix(&mut self) {
        if self.api_version.trim().is_empty() {
            tracing::warn!(target: "forge_vk", "forge-vk: empty api_version; falling back to default");
            self.api_version = default_api_version();
        }
        if self.base_url.trim().is_empty() {
            tracing::warn!(target: "forge_vk", "forge-vk: empty base_url; falling back to default");
            self.base_url = default_base_url();
        }
        // Хвостовой слэш ломал бы склейку `{base_url}/{method}` двойным
        // разделителем — VK на такой путь отвечает HTML-страницей 404, и
        // разбор конверта выдал бы бессмысленный `decode`-отказ.
        let trimmed = self.base_url.trim().trim_end_matches('/').to_string();
        self.base_url = trimmed;
        if self.max_upload_bytes == 0 {
            tracing::warn!(
                target: "forge_vk",
                "forge-vk: max_upload_bytes must be > 0; falling back to default"
            );
            self.max_upload_bytes = default_max_upload_bytes();
        }
        if self.rate_limit_rps == 0 || self.rate_limit_rps > MAX_RATE_LIMIT_RPS {
            tracing::warn!(
                target: "forge_vk",
                value = self.rate_limit_rps,
                "forge-vk: rate_limit_rps is out of 1..={MAX_RATE_LIMIT_RPS}; falling back to default"
            );
            self.rate_limit_rps = default_rate_limit_rps();
        }
        if self.longpoll_wait_secs == 0 || self.longpoll_wait_secs > MAX_LONGPOLL_WAIT_SECS {
            tracing::warn!(
                target: "forge_vk",
                value = self.longpoll_wait_secs,
                "forge-vk: longpoll_wait_secs is out of 1..={MAX_LONGPOLL_WAIT_SECS}; falling back to default"
            );
            self.longpoll_wait_secs = default_longpoll_wait_secs();
        }
        self.cache_dir = expand_tilde(&self.cache_dir);
        if let Err(e) = self.ensure_dirs() {
            tracing::warn!(
                target: "forge_vk",
                error = %e,
                "forge-vk: failed to create cache directory"
            );
        }
    }

    /// Создаёт `cache_dir`.
    ///
    /// # Errors
    ///
    /// Пробрасывает io-ошибку `create_dir_all` с путём в контексте.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", self.cache_dir.display()))?;
        Ok(())
    }

    /// Есть ли у плагина непустой токен.
    ///
    /// Из этого выводится начальное состояние логина: `true` ⇒
    /// [`crate::login::LoginFlow::Disconnected`] (токен есть, но он ещё не
    /// проверен сетью), `false` ⇒ `NotConfigured`.
    pub fn has_token(&self) -> bool {
        !self.access_token.is_empty()
    }

    /// Сохраняет токен в TOML-конфиг по пути [`config_path`].
    ///
    /// См. [`Self::save_token_to`] — здесь только подстановка пути.
    ///
    /// # Errors
    ///
    /// Любая ошибка записи; целевой файл при этом остаётся нетронутым.
    pub fn save_token(&mut self, token: &SecretToken) -> anyhow::Result<()> {
        let path = self.config_path.clone();
        self.save_token_to(&path, token)
    }

    /// Стирает токен из TOML-конфига по пути [`VkConfig::config_path`].
    ///
    /// См. [`Self::clear_token_to`] — здесь только подстановка пути.
    ///
    /// # Errors
    ///
    /// Ошибка чтения/записи конфига.
    pub fn clear_token(&mut self) -> anyhow::Result<()> {
        let path = self.config_path.clone();
        self.clear_token_to(&path)
    }

    /// Атомарно записывает `access_token` в TOML-конфиг по пути `path` и
    /// обновляет его в памяти.
    ///
    /// Инварианты (ради них функция и существует отдельно от `serde`):
    ///
    /// 1. **read-modify-write** — остальные поля пользовательского `vk.toml`
    ///    (`base_url`, лимиты и любые незнакомые ключи) сохраняются как есть;
    /// 2. **права `0600` выставляются в момент создания файла** — секрет ни на
    ///    мгновение не лежит на диске с более широкими правами;
    /// 3. **атомарность** — содержимое пишется во временный файл в том же
    ///    каталоге, `fsync`, затем `rename` поверх целевого: читатель видит
    ///    либо старый файл, либо новый, но никогда обрезанный;
    /// 4. при любой ошибке временный файл удаляется, целевой не меняется.
    ///
    /// Токен НИКОГДА не попадает в логи — пишется только отпечаток.
    ///
    /// # Errors
    ///
    /// Пустой токен; невозможность создать каталог/файл; ошибка записи,
    /// `fsync` или `rename`.
    pub fn save_token_to(&mut self, path: &Path, token: &SecretToken) -> anyhow::Result<()> {
        if token.is_empty() {
            anyhow::bail!("access_token must not be empty");
        }
        let mut doc = read_table(path)?;
        doc.insert(
            "access_token".into(),
            // ЕДИНСТВЕННОЕ место во всём крейте, где секрет превращается в
            // сериализуемое значение.
            toml::Value::String(token.expose().to_string()),
        );
        write_table(path, &doc)?;

        self.access_token = token.clone();
        tracing::info!(
            target: "forge_vk",
            path = %path.display(),
            token = %token.fingerprint(),
            "forge-vk: saved access token (only the fingerprint is ever logged)"
        );
        Ok(())
    }

    /// Убирает токен из конфига (logout) — и с диска, и из памяти.
    ///
    /// Остальные ключи пользователя переживают операцию; отсутствующий файл —
    /// не ошибка (выходить было не из чего, состояние всё равно обязано стать
    /// `not_configured`).
    ///
    /// # Errors
    ///
    /// Ошибка чтения/записи конфига.
    pub fn clear_token_to(&mut self, path: &Path) -> anyhow::Result<()> {
        self.access_token = SecretToken::default();
        let mut doc = read_table(path)?;
        if doc.remove("access_token").is_none() && !path.exists() {
            // Файла нет и стирать нечего — не создаём его ради пустоты.
            return Ok(());
        }
        write_table(path, &doc)?;
        tracing::info!(
            target: "forge_vk",
            path = %path.display(),
            "forge-vk: access token removed from the config"
        );
        Ok(())
    }
}

/// Read-modify-write первая половина: читает TOML-таблицу целиком.
fn read_table(path: &Path) -> anyhow::Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(toml::from_str(&body).unwrap_or_else(|e| {
            tracing::warn!(
                target: "forge_vk",
                path = %path.display(),
                error = %e,
                "forge-vk: vk.toml is not valid TOML; rewriting it from scratch"
            );
            toml::Table::new()
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(anyhow::anyhow!("failed to read {}: {e}", path.display())),
    }
}

/// Read-modify-write вторая половина: атомарная запись таблицы с правами 0600.
fn write_table(path: &Path, doc: &toml::Table) -> anyhow::Result<()> {
    let body = toml::to_string_pretty(doc)
        .map_err(|e| anyhow::anyhow!("failed to serialize vk.toml: {e}"))?;

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }

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
        // Никаких частичных состояний: убираем tmp, целевой файл остался
        // прежним.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Путь временного файла рядом с целевым: `.<имя>.<pid>.tmp`.
///
/// PID в имени защищает от коллизии двух процессов, пишущих конфиг
/// одновременно: последний `rename` выигрывает — это ожидаемое поведение, но
/// обрезанного файла не увидит никто.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "vk.toml".to_string());
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
    // mode() не применяется, поэтому права выставляются явно, и всё это ДО
    // первого write_all.
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

/// Чинит права существующего конфига, если они шире `0600`.
///
/// Вызывается на КАЖДОЙ загрузке, а не только при записи: пользователь мог
/// создать файл сам, скопировать его с другой машины или восстановить из
/// бэкапа с сохранением прав. Возвращает `true`, если что-то поменялось —
/// это нужно тесту, а не вызывающему коду.
fn tighten_perms_if_needed(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 == 0 {
            return false;
        }
        match set_owner_only(path) {
            Ok(()) => {
                tracing::warn!(
                    target: "forge_vk",
                    path = %path.display(),
                    mode = format!("{mode:o}"),
                    "forge-vk: vk.toml holds the access token and was group/world-readable; tightened to 0600"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_vk",
                    path = %path.display(),
                    error = %e,
                    "forge-vk: vk.toml has permissions wider than 0600 and could not be tightened"
                );
                false
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Разворачивает ведущий `~` в `$HOME`.
///
/// Нужен потому, что `cache_dir` пользователь пишет руками в `vk.toml`, а
/// TOML — не shell: без разворота `~/.forge/vk-cache` создал бы каталог с
/// именем `~` в текущей директории.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let rest = s.strip_prefix("~/").unwrap_or("");
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
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
                target: "forge_vk",
                key,
                value = %v,
                error = %e,
                "forge-vk: env value is not a u64; ignored"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Все env-тесты сериализуются между собой: `std::env` — process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const ENV_KEYS: &[&str] = &[
        "FORGE_VK_CONFIG_PATH",
        "FORGE_VK_ACCESS_TOKEN",
        "FORGE_VK_API_VERSION",
        "FORGE_VK_BASE_URL",
        "FORGE_VK_CACHE_DIR",
        "FORGE_VK_MAX_UPLOAD_BYTES",
        "FORGE_VK_MAX_CACHE_BYTES",
        "FORGE_VK_RATE_LIMIT_RPS",
        "FORGE_VK_LONGPOLL_WAIT_SECS",
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
        let missing = dir.path().join("nope/vk.toml");

        let mut cfg = VkConfig::load_from_file(&missing);
        cfg.apply_env();

        assert_eq!(cfg, VkConfig::default());
        assert!(!cfg.has_token());
        assert_eq!(cfg.api_version, DEFAULT_API_VERSION);
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert_eq!(cfg.rate_limit_rps, DEFAULT_RATE_LIMIT_RPS);
        assert_eq!(cfg.longpoll_wait_secs, DEFAULT_LONGPOLL_WAIT_SECS);
        clear_env();
    }

    #[test]
    fn toml_file_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            "access_token = \"vk1.a.fromfile\"\napi_version = \"5.131\"\nrate_limit_rps = 2\n",
        )
        .unwrap();

        let cfg = VkConfig::load_from_file(&path);
        assert!(cfg.has_token());
        assert_eq!(cfg.access_token.expose(), "vk1.a.fromfile");
        assert_eq!(cfg.api_version, "5.131");
        assert_eq!(cfg.rate_limit_rps, 2);
        // Незаданные поля — дефолты.
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
    }

    #[test]
    fn env_overrides_toml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            "access_token = \"from_toml\"\nbase_url = \"https://api.vk.com/method\"\nrate_limit_rps = 1\n",
        )
        .unwrap();

        std::env::set_var("FORGE_VK_ACCESS_TOKEN", "from_env");
        std::env::set_var("FORGE_VK_BASE_URL", "http://127.0.0.1:9999/method");
        std::env::set_var("FORGE_VK_RATE_LIMIT_RPS", "5");
        std::env::set_var("FORGE_VK_MAX_UPLOAD_BYTES", "1024");

        let mut cfg = VkConfig::load_from_file(&path);
        cfg.apply_env();

        assert_eq!(cfg.access_token.expose(), "from_env", "env перекрывает TOML");
        assert_eq!(cfg.base_url, "http://127.0.0.1:9999/method");
        assert_eq!(cfg.rate_limit_rps, 5);
        assert_eq!(cfg.max_upload_bytes, 1024);
        clear_env();
    }

    #[test]
    fn env_garbage_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("FORGE_VK_RATE_LIMIT_RPS", "three");
        std::env::set_var("FORGE_VK_MAX_CACHE_BYTES", "-1");
        std::env::set_var("FORGE_VK_LONGPOLL_WAIT_SECS", "18446744073709551616");

        let mut cfg = VkConfig::default();
        cfg.apply_env();

        assert_eq!(cfg.rate_limit_rps, DEFAULT_RATE_LIMIT_RPS);
        assert_eq!(cfg.max_cache_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert_eq!(cfg.longpoll_wait_secs, DEFAULT_LONGPOLL_WAIT_SECS);
        clear_env();
    }

    /// `export FORGE_VK_FOO=` (пустая строка) — частый артефакт shell-
    /// конфигов. Пустой env не должен затирать значения из TOML, и особенно
    /// не должен «разлогинивать» пользователя, обнулив токен.
    #[test]
    fn env_empty_values_do_not_override_toml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            "access_token = \"keep\"\napi_version = \"5.100\"\nmax_upload_bytes = 42\n",
        )
        .unwrap();
        for k in ENV_KEYS {
            std::env::set_var(k, "");
        }

        let mut cfg = VkConfig::load_from_file(&path);
        cfg.apply_env();

        assert_eq!(cfg.access_token.expose(), "keep");
        assert_eq!(cfg.api_version, "5.100");
        assert_eq!(cfg.max_upload_bytes, 42);
        clear_env();
    }

    #[test]
    fn validate_rejects_out_of_range_values() {
        let bad = [
            VkConfig {
                api_version: "  ".into(),
                ..VkConfig::default()
            },
            VkConfig {
                base_url: String::new(),
                ..VkConfig::default()
            },
            VkConfig {
                max_upload_bytes: 0,
                ..VkConfig::default()
            },
            VkConfig {
                rate_limit_rps: 0,
                ..VkConfig::default()
            },
            VkConfig {
                rate_limit_rps: MAX_RATE_LIMIT_RPS + 1,
                ..VkConfig::default()
            },
            VkConfig {
                longpoll_wait_secs: 0,
                ..VkConfig::default()
            },
            VkConfig {
                longpoll_wait_secs: MAX_LONGPOLL_WAIT_SECS + 1,
                ..VkConfig::default()
            },
        ];
        for cfg in bad {
            assert!(cfg.validate().is_err(), "должно быть невалидно: {cfg:?}");
        }
        assert!(VkConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_and_fix_clamps_and_creates_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = VkConfig {
            api_version: "   ".into(),
            base_url: "https://api.vk.com/method/".into(),
            max_upload_bytes: 0,
            rate_limit_rps: 9999,
            longpoll_wait_secs: 0,
            cache_dir: dir.path().join("nested/cache"),
            ..VkConfig::default()
        };
        cfg.validate_and_fix();

        assert_eq!(cfg.api_version, DEFAULT_API_VERSION);
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL, "хвостовой / срезан");
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.rate_limit_rps, DEFAULT_RATE_LIMIT_RPS);
        assert_eq!(cfg.longpoll_wait_secs, DEFAULT_LONGPOLL_WAIT_SECS);
        assert!(cfg.cache_dir.is_dir(), "cache_dir создан");
        assert!(cfg.validate().is_ok());
    }

    /// Минимально валидные значения (1 байт, 1 rps, 1 секунда ожидания)
    /// validate_and_fix трогать НЕ должен — ловит регресс `>=` vs `>`.
    #[test]
    fn validate_and_fix_keeps_minimal_valid_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = VkConfig {
            max_upload_bytes: 1,
            max_cache_bytes: 0, // 0 для кэша ВАЛИДЕН («не подрезать»)
            rate_limit_rps: 1,
            longpoll_wait_secs: 1,
            cache_dir: dir.path().join("cache"),
            ..VkConfig::default()
        };
        cfg.validate_and_fix();

        assert_eq!(cfg.max_upload_bytes, 1);
        assert_eq!(cfg.max_cache_bytes, 0);
        assert_eq!(cfg.rate_limit_rps, 1);
        assert_eq!(cfg.longpoll_wait_secs, 1);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn load_does_not_panic_without_file_and_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        // Полная изоляция от реального ~/.config/forge: конфиг и кэш смотрят
        // в tempdir (файла конфига там нет — это штатный путь).
        std::env::set_var("FORGE_VK_CONFIG_PATH", dir.path().join("vk.toml"));
        std::env::set_var("FORGE_VK_CACHE_DIR", dir.path().join("cache"));

        let cfg = VkConfig::load();
        assert!(cfg.validate().is_ok());
        assert!(
            !cfg.has_token(),
            "load() не должен подтянуть токен с реальной машины"
        );
        clear_env();
    }

    /// Битый TOML (обрыв строки, неверный тип поля) отвергается ЦЕЛИКОМ:
    /// частичное применение дало бы непредсказуемую смесь конфига и дефолтов.
    #[test]
    fn broken_toml_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            "access_token = \"vk1.a",
            "rate_limit_rps = \"three\"\naccess_token = \"t\"\n",
            "{not toml at all",
        ] {
            let path = dir.path().join("vk.toml");
            std::fs::write(&path, body).unwrap();
            let cfg = VkConfig::load_from_file(&path);
            assert_eq!(cfg, VkConfig::default(), "битый файл ⇒ только дефолты: {body}");
        }
    }

    /// Незнакомые ключи (конфиг от более новой версии devforge) не должны
    /// ронять парсинг.
    #[test]
    fn toml_unknown_keys_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            "access_token = \"t\"\nfuture_flag = true\n[future_section]\nx = 1\n",
        )
        .unwrap();

        let cfg = VkConfig::load_from_file(&path);
        assert_eq!(cfg.access_token.expose(), "t");
    }

    /// Пользователь по ошибке создал `~/.config/forge/vk.toml` КАТАЛОГОМ:
    /// read_to_string даёт не-NotFound ошибку — warn + дефолты, без паники.
    #[test]
    fn load_from_file_on_directory_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VkConfig::load_from_file(dir.path());
        assert_eq!(cfg, VkConfig::default());
    }

    /// systemd-юнит/контейнер без `$HOME`: дефолтные пути откатываются на
    /// относительные, а не паникуют на `var_os`.
    #[test]
    fn default_paths_fall_back_to_relative_without_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("HOME");
        std::env::remove_var("HOME");

        let cfg_path = default_config_path();
        let cache = default_cache_dir();

        if let Some(h) = saved_home {
            std::env::set_var("HOME", h);
        }
        assert_eq!(cfg_path, PathBuf::from("./vk.toml"));
        assert_eq!(cache, PathBuf::from("./vk-cache"));
    }

    // ------------------------------------------------------------------
    // Секрет: редактирование, права, атомарность.
    // ------------------------------------------------------------------

    /// ГЛАВНЫЙ инвариант модуля: `{:?}` конфига не раскрывает токен.
    ///
    /// Дополняет (а не заменяет) отсутствие `Serialize`: `Debug` выводится
    /// автоматически, поэтому единственная защита — ручная реализация у
    /// самого newtype.
    #[test]
    fn debug_of_config_is_redacted() {
        let cfg = VkConfig {
            access_token: SecretToken::new("vk1.a.SUPERSECRETVALUE"),
            ..VkConfig::default()
        };
        let dumped = format!("{cfg:?}");
        assert!(
            !dumped.contains("SUPERSECRET"),
            "токен утёк в Debug: {dumped}"
        );
        assert!(dumped.contains("<redacted>"), "{dumped}");

        // И сам newtype, и его Display-less вывод внутри других структур.
        assert_eq!(format!("{:?}", cfg.access_token), "<redacted>");
    }

    /// Отпечаток годится для лога и не раскрывает секрет: короткие строки не
    /// показываются вовсе, длинные — только края.
    #[test]
    fn fingerprint_never_reveals_the_whole_token() {
        assert_eq!(SecretToken::default().fingerprint(), "<none>");
        assert_eq!(SecretToken::new("   ").fingerprint(), "<none>");
        assert_eq!(SecretToken::new("short").fingerprint(), "<short>");

        let token = SecretToken::new("vk1.a.abcdefghijklmnop4f9c3f");
        let fp = token.fingerprint();
        assert_eq!(fp, "vk1.a…4f9c3f");
        assert!(!fp.contains("abcdefghijkl"), "середина обязана быть скрыта");
        assert!(fp.len() < token.expose().len());
    }

    /// Токен из буфера обмена почти всегда приезжает с пробелом или
    /// переносом строки — они обязаны срезаться на входе, иначе в
    /// `Authorization`-параметр уедет `\n` и VK ответит кодом 5.
    #[test]
    fn secret_token_trims_input() {
        assert_eq!(SecretToken::new("  vk1.a.x \n").expose(), "vk1.a.x");
        assert!(SecretToken::new("   ").is_empty());
        assert!(!SecretToken::new(" x ").is_empty());
    }

    #[test]
    fn save_token_writes_atomically_with_owner_only_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/vk.toml");

        let mut cfg = VkConfig::default();
        cfg.save_token_to(&path, &SecretToken::new("  vk1.a.written  "))
            .unwrap();

        // В памяти токен появился (с обрезанными пробелами).
        assert_eq!(cfg.access_token.expose(), "vk1.a.written");
        assert!(cfg.has_token());

        // На диске лежит валидный TOML с тем же значением.
        let reloaded = VkConfig::load_from_file(&path);
        assert_eq!(reloaded.access_token.expose(), "vk1.a.written");

        // Права ровно 0600.
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
    fn save_token_preserves_other_user_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(
            &path,
            "rate_limit_rps = 2\nbase_url = \"http://localhost:1/method\"\n\
             access_token = \"old\"\nsome_future_key = \"keep me\"\n",
        )
        .unwrap();

        let mut cfg = VkConfig::default();
        cfg.save_token_to(&path, &SecretToken::new("new")).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Table = toml::from_str(&body).unwrap();
        assert_eq!(doc["access_token"].as_str(), Some("new"));
        assert_eq!(doc["rate_limit_rps"].as_integer(), Some(2));
        assert_eq!(doc["base_url"].as_str(), Some("http://localhost:1/method"));
        assert_eq!(
            doc["some_future_key"].as_str(),
            Some("keep me"),
            "незнакомые ключи пользователя не должны исчезать"
        );
    }

    #[test]
    fn save_token_rejects_empty_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");

        let mut cfg = VkConfig::default();
        assert!(cfg.save_token_to(&path, &SecretToken::new("   ")).is_err());
        assert!(!cfg.has_token(), "память не изменилась");
        assert!(!path.exists(), "файл не создан");
    }

    /// Logout: токен уходит и с диска, и из памяти, остальные ключи живы.
    #[test]
    fn clear_token_removes_only_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        let mut cfg = VkConfig::default();
        cfg.save_token_to(&path, &SecretToken::new("bye")).unwrap();

        // Дописываем пользовательский ключ, чтобы проверить его выживание.
        let mut doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc.insert("rate_limit_rps".into(), toml::Value::Integer(2));
        std::fs::write(&path, toml::to_string_pretty(&doc).unwrap()).unwrap();

        cfg.clear_token_to(&path).unwrap();
        assert!(!cfg.has_token());
        let reloaded = VkConfig::load_from_file(&path);
        assert!(!reloaded.has_token(), "токен стёрт с диска");
        assert_eq!(reloaded.rate_limit_rps, 2, "чужие ключи уцелели");
    }

    /// Logout без конфига — не ошибка и не повод создавать пустой файл.
    #[test]
    fn clear_token_on_missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        let mut cfg = VkConfig::default();
        cfg.clear_token_to(&path).unwrap();
        assert!(!path.exists());
        assert!(!cfg.has_token());
    }

    /// ДОБАВЛЕНИЕ ПРОТИВ TELEGRAM: файл с токеном и правами 0644 при загрузке
    /// чинится до 0600. Пользователь создаёт `vk.toml` руками чаще, чем можно
    /// надеяться, и его umask нам не подчиняется.
    #[cfg(unix)]
    #[test]
    fn loading_tightens_world_readable_config() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::write(&path, "access_token = \"vk1.a.handmade\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let cfg = VkConfig::load_from_file(&path);

        assert_eq!(cfg.access_token.expose(), "vk1.a.handmade", "файл прочитан");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "права обязаны быть ужаты, получили {mode:o}");
    }

    /// Права ровно 0600 (и более узкие, 0400) трогать не нужно: лишний chmod
    /// в штатном пути — это лишний warn в логе на каждом старте.
    #[cfg(unix)]
    #[test]
    fn loading_leaves_already_private_config_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for mode in [0o600, 0o400] {
            let path = dir.path().join(format!("vk{mode:o}.toml"));
            std::fs::write(&path, "access_token = \"t\"\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(!tighten_perms_if_needed(&path), "{mode:o}: чинить нечего");
            let after = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(after, mode, "права не должны меняться");
        }
    }

    /// Сценарий после падения процесса: от прошлого запуска остался tmp-файл
    /// с широкими правами. `write_secret_file` открывает его через create (не
    /// create_new), поэтому обязан явно ужать права ДО записи секрета — иначе
    /// токен полежал бы world-readable.
    #[cfg(unix)]
    #[test]
    fn save_token_tightens_perms_of_stale_tmp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        let tmp = tmp_path_for(&path);
        std::fs::write(&tmp, b"stale garbage from a crashed process").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();

        let mut cfg = VkConfig::default();
        cfg.save_token_to(&path, &SecretToken::new("secret")).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "унаследованные 0666 обязаны ужаться");
        assert_eq!(
            VkConfig::load_from_file(&path).access_token.expose(),
            "secret"
        );
        assert!(!tmp.exists(), "tmp переименован, мусора нет");
    }

    /// Каталог конфига без права записи: ошибка наружу, целевой файл не
    /// появился, tmp-мусор не остался, память не изменилась.
    #[cfg(unix)]
    #[test]
    fn save_token_to_read_only_dir_fails_without_leftovers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("cfg");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = parent.join("vk.toml");

        let mut cfg = VkConfig::default();
        let res = cfg.save_token_to(&path, &SecretToken::new("hash"));

        // Возвращаем права до assert'ов, чтобы TempDir убрался при панике.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(res.is_err());
        assert!(!cfg.has_token());
        assert!(!path.exists(), "целевой файл не должен появиться");
        let leftovers: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(leftovers.is_empty(), "остался мусор: {leftovers:?}");
    }

    /// Целевой путь занят каталогом — save обязан вернуть ошибку, не изменив
    /// ни память, ни ФС.
    #[test]
    fn save_token_to_fails_when_path_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vk.toml");
        std::fs::create_dir(&path).unwrap();

        let mut cfg = VkConfig::default();
        assert!(cfg.save_token_to(&path, &SecretToken::new("t")).is_err());
        assert!(!cfg.has_token(), "память не изменилась при ошибке");
        assert!(path.is_dir(), "каталог не тронут");
    }

    /// `~` в `cache_dir` пишется руками в TOML — без разворота получился бы
    /// каталог с именем `~` в рабочей директории процесса.
    #[test]
    fn cache_dir_tilde_is_expanded() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let mut cfg = VkConfig {
            cache_dir: PathBuf::from("~/.forge/vk-cache"),
            ..VkConfig::default()
        };
        cfg.validate_and_fix();
        let expanded = cfg.cache_dir.clone();

        match saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(expanded, dir.path().join(".forge/vk-cache"));
        assert!(expanded.is_dir());
    }

    /// `FORGE_VK_CONFIG_PATH` переопределяет и чтение, и запись — всегда один
    /// файл. Без этого `cargo test` затирал бы живой `~/.config/forge/vk.toml`.
    #[test]
    fn config_path_env_overrides_load_and_save() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("nested/vk.toml");
        std::env::set_var("FORGE_VK_CONFIG_PATH", &toml_path);
        std::env::set_var("FORGE_VK_CACHE_DIR", dir.path().join("cache"));

        assert_eq!(config_path(), toml_path);

        let mut cfg = VkConfig::load();
        assert!(!cfg.has_token(), "файла ещё нет — токена нет");

        cfg.save_token(&SecretToken::new("vk1.a.viaenv")).unwrap();
        assert!(toml_path.is_file(), "save пишет по env-пути, не в $HOME");

        let reloaded = VkConfig::load();
        assert_eq!(reloaded.access_token.expose(), "vk1.a.viaenv");
        clear_env();
    }
}
