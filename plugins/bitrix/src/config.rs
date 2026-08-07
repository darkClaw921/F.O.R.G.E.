//! Конфигурация плагина Bitrix24 и хранение секрета вебхука.
//!
//! # Что здесь секрет
//!
//! Секрет Bitrix24 — **весь URL входящего вебхука целиком**:
//! `https://<portal>/rest/<user_id>/<code>/`. Код криптографически связан с
//! `user_id`, поэтому «спрятать только код» нельзя: пара `user_id + code` и
//! есть учётные данные. Отсюда тип [`WebhookUrl`] — newtype **без
//! `Serialize`**: любая попытка положить его в DTO, WS-кадр или структурный
//! лог перестаёт компилироваться. Это дешевле дисциплины и теста на каждое
//! новое поле.
//!
//! Читать содержимое можно только через [`WebhookUrl::expose`] — grep по
//! `expose()` показывает ВСЕ места, где секрет покидает newtype.
//!
//! # Где живёт файл
//!
//! `~/.config/forge/bitrix.toml`, права `0600` (Р5). В `user_settings.json`
//! вебхука нет и не будет: тот файл читается фронтом и уезжает в браузер.
//! Путь переопределяется непустым `FORGE_BITRIX_CONFIG_PATH` — без этого
//! `cargo test` затирал бы живой вебхук разработчика.
//!
//! # Как секрет попадает на диск (Ф3)
//!
//! Ровно одним путём — [`save_webhook_to`], и у него четыре инварианта:
//!
//! 1. **read-modify-write** — остальные ключи пользовательского
//!    `bitrix.toml` (интервалы, лимиты, любые незнакомые) переживают запись;
//! 2. **права `0600` выставляются В МОМЕНТ создания файла**
//!    ([`write_secret_file`]) — секрет ни на мгновение не лежит на диске с
//!    более широкими правами;
//! 3. **атомарность** — содержимое пишется во временный файл в том же
//!    каталоге, `fsync`, затем `rename` поверх целевого: читатель видит либо
//!    старый файл, либо новый, но никогда обрезанный;
//! 4. при любой ошибке временный файл удаляется, а целевой остаётся прежним.
//!
//! Плюс [`tighten_perms_if_needed`] на КАЖДОЙ загрузке: файл могли создать
//! руками через `echo >`, скопировать с другой машины или достать из бэкапа —
//! тогда `0644` чинится на `0600` с warn'ом.
//!
//! Мультипортальности в v1 нет (Р5): поле вебхука одно, повторное сохранение
//! перезаписывает его.
//!
//! # Интервалы — данные, а не константы (Р4)
//!
//! `poll_active_secs` / `poll_idle_secs` / `recent_sync_secs` лежат в
//! конфиге именно затем, чтобы тесты Фазы 7 могли инжектировать сотни
//! миллисекунд вместо боевых секунд и не спать минутами.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Интервал опроса `im.v2.Event.get`, когда вкладка открыта (есть
/// WS-подписчики), в секундах (Р4).
pub const DEFAULT_POLL_ACTIVE_SECS: u64 = 4;

/// Интервал опроса, когда WS-подписчиков нет: вкладку закрыли, но бейдж и
/// тосты обязаны продолжать работать — просто реже (Р4).
pub const DEFAULT_POLL_IDLE_SECS: u64 = 20;

/// Период фолбэк-синхронизации `im.recent.get{LAST_SYNC_DATE}` +
/// `im.counters.get` (Р4). Единственный источник правок сообщений: событие
/// `ONIMV2MESSAGEUPDATE` документировано, но живьём не приходит.
pub const DEFAULT_RECENT_SYNC_SECS: u64 = 60;

/// Потолок размера отправляемого файла — **25 MiB** (Р7).
///
/// Платформенный лимит вдвое-вчетверо больше (100 MiB), но заливка идёт
/// одношагово через base64 в JSON-теле: 100 МБ файла — это ~133 МБ строки в
/// памяти процесса на каждую параллельную отправку. 25 MiB покрывают
/// реальную переписку и держат RSS в рамках.
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

/// Дисковый бюджет файлового кэша медиа — **512 MiB** (Ф8).
///
/// Столько же, сколько у панелей VK и Telegram: кэш живёт в домашнем каталоге
/// пользователя, и три панели на одной машине не должны суммарно съедать
/// больше пары гигабайт.
///
/// Значение `0` означает «не подрезать» — легальный режим для того, кто держит
/// кэш на отдельном томе и не хочет терять уже скачанное.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Клиентский лимит интенсивности: портал даёт 2 req/s на пару портал+IP.
pub const DEFAULT_RATE_LIMIT_RPS: u32 = 2;

/// Ёмкость token bucket: залп из пяти запросов проходит мгновенно, дальше —
/// ровно `1/rps`. Портальное ведро вместимостью 50, наше намеренно меньше:
/// девять десятых бюджета остаются живому пользователю.
pub const DEFAULT_BURST: u32 = 5;

/// Верхняя граница `rate_limit_rps`: выше портал начинает рвать соединения
/// без JSON-ошибки, и «ускорение» превращается в зависания.
pub const MAX_RATE_LIMIT_RPS: u32 = 10;

/// Верхняя граница `burst`.
pub const MAX_BURST: u32 = 50;

/// Верхняя граница любого из интервалов опроса (секунды).
pub const MAX_POLL_SECS: u64 = 3600;

/// Таймаут установления TCP+TLS-соединения с порталом (секунды).
///
/// Отделён от общего таймаута запроса намеренно: он отличает «сети нет» от
/// «портал думает», и первое обязано диагностироваться за секунды, а не за
/// полминуты.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Общий таймаут вызова метода портала (секунды).
///
/// **Обязателен, а не желателен.** При превышении лимита интенсивности портал
/// не отвечает JSON-ошибкой, а ЗАВИСАЕТ или рвёт TCP (см. §4 плана): без
/// клиентского таймаута такой вызов держал бы задачу неопределённо долго, а
/// повторить его было бы нечему.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Верхняя граница любого из таймаутов (секунды).
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// Дефолтный путь TOML-конфига плагина (`~/.config/forge/bitrix.toml`).
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config/forge/bitrix.toml")
    } else {
        PathBuf::from("./bitrix.toml")
    }
}

/// Фактический путь TOML-конфига: непустой `FORGE_BITRIX_CONFIG_PATH`
/// побеждает, иначе — [`default_config_path`].
///
/// Env-override нужен контейнерам с нестандартным layout'ом и герметичным
/// тестам: без него [`BitrixConfig::load`] всегда читал бы реальный
/// `~/.config/forge/bitrix.toml` машины разработчика, а запись вебхука (Ф3)
/// туда же — то есть прогон `cargo test` разлогинивал бы пользователя.
///
/// Он же — признак ТЕСТОВОГО режима для правила Р17: `http://127.0.0.1` в
/// `base_url_override` разрешён только при непустом значении этой переменной.
pub fn config_path() -> PathBuf {
    match std::env::var_os("FORGE_BITRIX_CONFIG_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_config_path(),
    }
}

/// Установлен ли непустой `FORGE_BITRIX_CONFIG_PATH` — то есть работает ли
/// плагин в изолированном (тестовом) окружении.
///
/// Единственный потребитель — правило Р17 (Ф2): `http://` в
/// `base_url_override` законен только здесь.
pub fn is_isolated_env() -> bool {
    matches!(std::env::var_os("FORGE_BITRIX_CONFIG_PATH"), Some(v) if !v.is_empty())
}

/// Дефолтный каталог файлового кэша (`~/.forge/bitrix-cache`).
///
/// Здесь живут байты вложений (Ф8), staging загрузок (Ф9) и курсор событий
/// `events.json` (Ф7).
pub fn default_cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge/bitrix-cache")
    } else {
        PathBuf::from("./bitrix-cache")
    }
}

fn default_poll_active_secs() -> u64 {
    DEFAULT_POLL_ACTIVE_SECS
}

fn default_poll_idle_secs() -> u64 {
    DEFAULT_POLL_IDLE_SECS
}

fn default_recent_sync_secs() -> u64 {
    DEFAULT_RECENT_SYNC_SECS
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

fn default_burst() -> u32 {
    DEFAULT_BURST
}

fn default_connect_timeout_secs() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

fn default_request_timeout_secs() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}

/// Проверка правила **Р17** для `base_url_override` — ЧИСТАЯ функция.
///
/// `isolated` — признак тестового окружения (непустой
/// `FORGE_BITRIX_CONFIG_PATH`, см. [`is_isolated_env`]). Параметром, а не
/// чтением env внутри: правило тогда проверяется юнит-тестами обеих веток без
/// глобальной подмены переменных процесса.
///
/// Разрешено:
/// - пустая строка (переопределения нет — база берётся из самого вебхука);
/// - любой `https://…` адрес;
/// - `http://` **только** на петлевой хост (`127.0.0.1`, `localhost`, `[::1]`)
///   и **только** в изолированном окружении.
///
/// # Почему именно так
///
/// Вебхук — это секрет, который клиент кладёт в ПУТЬ каждого запроса. Схема
/// `http` означает, что весь путь (то есть учётные данные портала) уезжает по
/// сети открытым текстом. Петлевой адрес физически не покидает машину, поэтому
/// он безопасен — но только он и только там, где мы точно знаем, что это
/// прогон тестов, а не боевая конфигурация с опечаткой в схеме.
///
/// # Errors
///
/// Возвращает человекочитаемое пояснение, пригодное и для лога, и для UI.
/// Само значение в текст ошибки НЕ подставляется: `base_url_override` может
/// содержать полный путь вебхука вместе с кодом.
pub fn check_base_url_override(raw: &str, isolated: bool) -> Result<(), String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(());
    }
    if let Some(host) = s.strip_prefix("https://") {
        if host.is_empty() {
            return Err("base_url_override: https URL without a host".to_string());
        }
        return Ok(());
    }
    let Some(rest) = s.strip_prefix("http://") else {
        return Err(
            "base_url_override must start with `https://` (or `http://127.0.0.1` in tests)"
                .to_string(),
        );
    };
    // Хост — всё до первого `/`; порт отбрасывается (`127.0.0.1:53219`).
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // IPv6-литерал: `[::1]:1234`.
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
    if !loopback {
        return Err(
            "base_url_override: plain http is allowed only for the loopback host (127.0.0.1)"
                .to_string(),
        );
    }
    if !isolated {
        return Err(
            "base_url_override: plain http://127.0.0.1 requires a non-empty FORGE_BITRIX_CONFIG_PATH (test mode)"
                .to_string(),
        );
    }
    Ok(())
}

/// URL входящего вебхука Bitrix24 — **секрет целиком**.
///
/// Форма: `https://<portal>/rest/<user_id>/<code>/`. Разбор трёх форм ввода
/// (с методом, без метода, с хвостовым слэшем и без) живёт в `login.rs` (Ф3)
/// и только на бэкенде.
///
/// `Serialize` не реализован и реализован не будет — см. шапку модуля.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct WebhookUrl(String);

impl WebhookUrl {
    /// Оборачивает строку, обрезая пробелы по краям (пользователь вставляет
    /// URL из буфера, где легко захватить перенос строки).
    pub fn new(raw: impl AsRef<str>) -> Self {
        Self(raw.as_ref().trim().to_string())
    }

    /// ЕДИНСТВЕННАЯ точка чтения секрета.
    ///
    /// Намеренно называется «expose», а не `as_str`: вызов должен выглядеть
    /// подозрительно в code review, и по нему же ищет тест-страж
    /// `no_source_line_logs_the_exposed_url` (Ф2).
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Пуст ли вебхук (то есть плагин не подключён к порталу).
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Разбирает URL на составляющие: `(origin, user_id, code)`.
    ///
    /// Регексп контракта (§4 плана):
    /// `^(https?://[^/\s]+)/rest/(\d+)/([A-Za-z0-9]+)/?$` — разобран вручную,
    /// чтобы не тащить крейт `regex` ради одной строки.
    ///
    /// Возвращает `None`, если строка не похожа на вебхук: вызывающий
    /// трактует это как `invalid_url` (Ф3).
    pub fn parts(&self) -> Option<(&str, &str, &str)> {
        let s = self.0.trim().trim_end_matches('/');
        let rest = s.find("/rest/")?;
        let (origin, tail) = s.split_at(rest);
        if !(origin.starts_with("https://") || origin.starts_with("http://")) {
            return None;
        }
        // Хост непуст и не содержит слэшей: `https:///rest/1/x` — не вебхук.
        let host = origin
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
            return None;
        }
        let tail = tail.strip_prefix("/rest/")?;
        let mut it = tail.split('/');
        let user_id = it.next()?;
        let code = it.next()?;
        if it.next().is_some() {
            return None;
        }
        if user_id.is_empty() || !user_id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if code.is_empty() || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        Some((origin, user_id, code))
    }

    /// Базовый URL для вызова методов: `<origin>/rest/<uid>/<code>` без
    /// хвостового слэша. Метод дописывается как `/<method>.json`.
    ///
    /// СОДЕРЖИТ СЕКРЕТ — наружу (в HTTP-ответ, в лог) не уходит никогда.
    pub fn api_base(&self) -> Option<String> {
        let (origin, uid, code) = self.parts()?;
        Some(format!("{origin}/rest/{uid}/{code}"))
    }

    /// Хост портала без схемы — единственная часть вебхука, которую МОЖНО
    /// показать пользователю: он и так видит её в адресной строке Bitrix24.
    ///
    /// Используется экраном подключения («вы подключены к test.bitrix24.ru»),
    /// но НЕ в `GET /api/bitrix/status` (см. `routes::StatusDto`).
    pub fn portal_host(&self) -> Option<String> {
        let (origin, _, _) = self.parts()?;
        Some(
            origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string(),
        )
    }

    /// Схема и хост портала: `https://test.bitrix24.ru`.
    ///
    /// Нужен ровно одному потребителю — ссылке «открыть в Битриксе» в карточке
    /// задачи (Ф10). Без схемы такая ссылка не открывается, а собирать её во
    /// фронте нечем: хост туда не уезжает больше нигде.
    ///
    /// **Кода вебхука здесь нет и быть не может** — только origin. Ограничение
    /// то же, что у [`WebhookUrl::portal_host`]: хост это единственная часть
    /// вебхука, которую можно показывать пользователю, потому что он и так
    /// видит её в адресной строке Bitrix24.
    pub fn portal_origin(&self) -> Option<String> {
        let (origin, _, _) = self.parts()?;
        Some(origin.to_string())
    }

    /// Отпечаток для ЛОГОВ: `test.bitrix24.ru#1…hmn83d`.
    ///
    /// Нужен ровно для одного вопроса — «тот ли вебхук вставлен» — когда у
    /// пользователя их несколько. В HTTP-ответы отпечаток НЕ кладётся: любая
    /// часть секрета в ответе — это часть секрета.
    ///
    /// Нераспознанная строка не раскрывается вовсе: если разбор не удался,
    /// мы не знаем, где в ней код, и печатать её опасно.
    pub fn fingerprint(&self) -> String {
        if self.is_empty() {
            return "<none>".to_string();
        }
        let Some((origin, uid, code)) = self.parts() else {
            return "<malformed>".to_string();
        };
        let host = origin
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let chars: Vec<char> = code.chars().collect();
        if chars.len() < 12 {
            return format!("{host}#{uid}…<short>");
        }
        let tail: String = chars[chars.len() - 6..].iter().collect();
        format!("{host}#{uid}…{tail}")
    }
}

/// `Debug` НИКОГДА не печатает содержимое: `{:?}` на конфиге, на состоянии
/// или в `tracing::debug!` не должен раскрывать вебхук ни при каких условиях.
impl fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<&str> for WebhookUrl {
    fn from(v: &str) -> Self {
        Self::new(v)
    }
}

impl From<String> for WebhookUrl {
    fn from(v: String) -> Self {
        Self::new(v)
    }
}

/// Конфигурация плагина.
///
/// Все поля имеют `#[serde(default)]`: пользователь правит только то, что ему
/// нужно, а отсутствующий файл эквивалентен полным дефолтам.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BitrixConfig {
    /// URL входящего вебхука. Пусто ⇒ состояние `not_configured`.
    pub webhook: WebhookUrl,

    /// Переопределение базового URL портала — ЕДИНСТВЕННЫЙ способ
    /// протестировать транспорт без живого портала (стенд `tests/b24_stub.rs`,
    /// Ф2).
    ///
    /// Пусто ⇒ база берётся из самого вебхука ([`WebhookUrl::api_base`]).
    /// Правило Р17 (проверяется в Ф2): в проде обязательна схема `https`,
    /// `http://127.0.0.1` разрешён только при непустом
    /// `FORGE_BITRIX_CONFIG_PATH` (см. [`is_isolated_env`]).
    pub base_url_override: String,

    /// Каталог файлового кэша: байты вложений, staging загрузок, курсор
    /// событий.
    pub cache_dir: PathBuf,

    /// Интервал опроса событий при наличии WS-подписчиков, секунды (Р4).
    #[serde(default = "default_poll_active_secs")]
    pub poll_active_secs: u64,

    /// Интервал опроса событий без WS-подписчиков, секунды (Р4).
    #[serde(default = "default_poll_idle_secs")]
    pub poll_idle_secs: u64,

    /// Период фолбэк-синхронизации списка и счётчиков, секунды (Р4).
    #[serde(default = "default_recent_sync_secs")]
    pub recent_sync_secs: u64,

    /// Потолок размера отправляемого файла, байты (Р7).
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,

    /// Дисковый бюджет кэша медиа, байты (Ф8). `0` ⇒ не подрезать.
    ///
    /// В отличие от [`Self::max_upload_bytes`] нулевое значение здесь
    /// ЗАКОННО и валидацией не чинится: «кэш не ограничен» — осмысленный
    /// выбор, а «файл нельзя отправить» — нет.
    #[serde(default = "default_max_cache_bytes")]
    pub max_cache_bytes: u64,

    /// Клиентский лимит интенсивности, запросов в секунду.
    #[serde(default = "default_rate_limit_rps")]
    pub rate_limit_rps: u32,

    /// Ёмкость token bucket (залп).
    #[serde(default = "default_burst")]
    pub burst: u32,

    /// Таймаут установления соединения с порталом, секунды.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,

    /// Общий таймаут вызова метода портала, секунды.
    ///
    /// Без него превышение лимита интенсивности (портал просто перестаёт
    /// отвечать) вешало бы вызов навсегда — ретраить было бы нечего.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// Путь файла, из которого конфиг прочитан.
    ///
    /// `#[serde(skip)]` — из TOML НЕ читается: записывать вебхук нужно ровно
    /// туда, откуда он был прочитан, иначе logout стирал бы его не из того
    /// файла.
    #[serde(skip)]
    pub config_path: PathBuf,
}

impl Default for BitrixConfig {
    fn default() -> Self {
        Self {
            webhook: WebhookUrl::default(),
            base_url_override: String::new(),
            cache_dir: default_cache_dir(),
            poll_active_secs: DEFAULT_POLL_ACTIVE_SECS,
            poll_idle_secs: DEFAULT_POLL_IDLE_SECS,
            recent_sync_secs: DEFAULT_RECENT_SYNC_SECS,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            rate_limit_rps: DEFAULT_RATE_LIMIT_RPS,
            burst: DEFAULT_BURST,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            config_path: config_path(),
        }
    }
}

impl BitrixConfig {
    /// Полная загрузка: defaults → optional TOML → env-override →
    /// validate_and_fix.
    ///
    /// Никогда не паникует и не возвращает ошибку: невалидные значения
    /// чинятся дефолтами с warn-логом. Отсутствие файла — штатный путь: у
    /// большинства пользователей `bitrix.toml` не существует, вебхук
    /// вставляется в UI.
    pub fn load() -> Self {
        let path = config_path();
        let mut cfg = Self::load_from_file(&path);
        // Читаем и пишем ОДИН файл: иначе logout стирал бы вебхук не оттуда,
        // откуда он был прочитан.
        cfg.config_path = path;
        cfg.apply_env();
        cfg.validate_and_fix();
        cfg
    }

    /// Читает конфиг из TOML-файла.
    ///
    /// Файла нет — дефолты (не ошибка). Файл битый — warn и ПОЛНЫЕ дефолты:
    /// частичного применения нет, иначе половина конфига оказалась бы из
    /// файла, а половина — из головы, и объяснить поведение стало бы нечем.
    pub fn load_from_file(path: &Path) -> Self {
        // Права чинятся ДО чтения: файл мог быть создан руками (`echo >`),
        // скопирован с другой машины или восстановлен из архива, и тогда
        // секрет уже лежит доступным для чтения всей группе.
        tighten_perms_if_needed(path);
        match std::fs::read_to_string(path) {
            Ok(body) => match toml::from_str::<BitrixConfig>(&body) {
                Ok(c) => {
                    tracing::info!(
                        target: "forge_bitrix",
                        path = %path.display(),
                        "forge-bitrix: loaded config from TOML"
                    );
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        target: "forge_bitrix",
                        path = %path.display(),
                        error = %e,
                        "forge-bitrix: failed to parse bitrix.toml; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    "forge-bitrix: bitrix.toml not found, using defaults"
                );
                Self::default()
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    error = %e,
                    "forge-bitrix: failed to read bitrix.toml; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Применяет env-override `FORGE_BITRIX_*`.
    ///
    /// Пустые строки и нечисловой мусор игнорируются с warn-логом: env не
    /// должен уметь «сломать» конфиг. Значение `FORGE_BITRIX_WEBHOOK` в лог не
    /// попадает никогда — даже как факт длины.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FORGE_BITRIX_WEBHOOK") {
            if !v.trim().is_empty() {
                self.webhook = WebhookUrl::new(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_BASE_URL") {
            if !v.trim().is_empty() {
                self.base_url_override = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_CACHE_DIR") {
            if !v.is_empty() {
                self.cache_dir = PathBuf::from(v);
            }
        }
        apply_env_u64("FORGE_BITRIX_POLL_ACTIVE_SECS", &mut self.poll_active_secs);
        apply_env_u64("FORGE_BITRIX_POLL_IDLE_SECS", &mut self.poll_idle_secs);
        apply_env_u64("FORGE_BITRIX_RECENT_SYNC_SECS", &mut self.recent_sync_secs);
        apply_env_u64("FORGE_BITRIX_MAX_UPLOAD_BYTES", &mut self.max_upload_bytes);
        apply_env_u64("FORGE_BITRIX_MAX_CACHE_BYTES", &mut self.max_cache_bytes);
        apply_env_u32("FORGE_BITRIX_RATE_LIMIT_RPS", &mut self.rate_limit_rps);
        apply_env_u32("FORGE_BITRIX_BURST", &mut self.burst);
        apply_env_u64(
            "FORGE_BITRIX_CONNECT_TIMEOUT_SECS",
            &mut self.connect_timeout_secs,
        );
        apply_env_u64(
            "FORGE_BITRIX_REQUEST_TIMEOUT_SECS",
            &mut self.request_timeout_secs,
        );
    }

    /// Проверяет инварианты конфига, НЕ меняя его.
    ///
    /// # Errors
    ///
    /// - `max_upload_bytes == 0` (иначе ни один файл не отправить);
    /// - `rate_limit_rps` вне `1..=MAX_RATE_LIMIT_RPS`;
    /// - `burst` вне `1..=MAX_BURST`;
    /// - любой интервал опроса вне `1..=MAX_POLL_SECS`;
    /// - любой таймаут вне `1..=MAX_TIMEOUT_SECS`;
    /// - `base_url_override` нарушает правило Р17 (см.
    ///   [`check_base_url_override`]).
    ///
    /// Пустой `webhook` ошибкой НЕ является: «плагин включён, но ещё не
    /// подключён» — штатное стартовое состояние.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_upload_bytes == 0 {
            anyhow::bail!("max_upload_bytes must be greater than zero");
        }
        if self.rate_limit_rps == 0 || self.rate_limit_rps > MAX_RATE_LIMIT_RPS {
            anyhow::bail!("rate_limit_rps must be within 1..={MAX_RATE_LIMIT_RPS}");
        }
        if self.burst == 0 || self.burst > MAX_BURST {
            anyhow::bail!("burst must be within 1..={MAX_BURST}");
        }
        for (name, v) in [
            ("poll_active_secs", self.poll_active_secs),
            ("poll_idle_secs", self.poll_idle_secs),
            ("recent_sync_secs", self.recent_sync_secs),
        ] {
            if v == 0 || v > MAX_POLL_SECS {
                anyhow::bail!("{name} must be within 1..={MAX_POLL_SECS}");
            }
        }
        for (name, v) in [
            ("connect_timeout_secs", self.connect_timeout_secs),
            ("request_timeout_secs", self.request_timeout_secs),
        ] {
            if v == 0 || v > MAX_TIMEOUT_SECS {
                anyhow::bail!("{name} must be within 1..={MAX_TIMEOUT_SECS}");
            }
        }
        if let Err(e) = check_base_url_override(&self.base_url_override, is_isolated_env()) {
            anyhow::bail!("{e}");
        }
        Ok(())
    }

    /// Чинит невалидные значения дефолтами и раскрывает `~` в `cache_dir`.
    ///
    /// В отличие от [`Self::validate`] ничего не возвращает: конфиг после
    /// вызова гарантированно валиден.
    pub fn validate_and_fix(&mut self) {
        fix_u64(
            "poll_active_secs",
            &mut self.poll_active_secs,
            1,
            MAX_POLL_SECS,
            DEFAULT_POLL_ACTIVE_SECS,
        );
        fix_u64(
            "poll_idle_secs",
            &mut self.poll_idle_secs,
            1,
            MAX_POLL_SECS,
            DEFAULT_POLL_IDLE_SECS,
        );
        fix_u64(
            "recent_sync_secs",
            &mut self.recent_sync_secs,
            1,
            MAX_POLL_SECS,
            DEFAULT_RECENT_SYNC_SECS,
        );
        if self.max_upload_bytes == 0 {
            tracing::warn!(
                target: "forge_bitrix",
                "forge-bitrix: max_upload_bytes = 0; falling back to the default"
            );
            self.max_upload_bytes = DEFAULT_MAX_UPLOAD_BYTES;
        }
        if self.rate_limit_rps == 0 || self.rate_limit_rps > MAX_RATE_LIMIT_RPS {
            tracing::warn!(
                target: "forge_bitrix",
                value = self.rate_limit_rps,
                "forge-bitrix: rate_limit_rps out of range; falling back to the default"
            );
            self.rate_limit_rps = DEFAULT_RATE_LIMIT_RPS;
        }
        if self.burst == 0 || self.burst > MAX_BURST {
            tracing::warn!(
                target: "forge_bitrix",
                value = self.burst,
                "forge-bitrix: burst out of range; falling back to the default"
            );
            self.burst = DEFAULT_BURST;
        }
        fix_u64(
            "connect_timeout_secs",
            &mut self.connect_timeout_secs,
            1,
            MAX_TIMEOUT_SECS,
            DEFAULT_CONNECT_TIMEOUT_SECS,
        );
        fix_u64(
            "request_timeout_secs",
            &mut self.request_timeout_secs,
            1,
            MAX_TIMEOUT_SECS,
            DEFAULT_REQUEST_TIMEOUT_SECS,
        );
        self.base_url_override = self.base_url_override.trim().trim_end_matches('/').to_string();
        // Р17: нарушение схемы не «чинится» — переопределение снимается
        // целиком. Тихо подставить https к чужому хосту было бы хуже, чем
        // вернуться к базе из самого вебхука: пользователь получил бы рабочий
        // плагин, ходящий не туда, куда он просил.
        if let Err(e) = check_base_url_override(&self.base_url_override, is_isolated_env()) {
            tracing::warn!(
                target: "forge_bitrix",
                error = %e,
                "forge-bitrix: base_url_override rejected; falling back to the webhook base"
            );
            self.base_url_override.clear();
        }
        self.cache_dir = expand_tilde(&self.cache_dir);
        if self.config_path.as_os_str().is_empty() {
            self.config_path = config_path();
        }
    }

    /// Создаёт каталоги кэша.
    ///
    /// # Errors
    ///
    /// Пробрасывает ошибку ФС. Вызывающий (`init`) трактует её как
    /// НЕ фатальную: каталог понадобится только при записи, и там ошибка
    /// всплывёт с более точным контекстом.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }

    /// Подключён ли вебхук прямо сейчас (по состоянию файла на старте).
    pub fn has_webhook(&self) -> bool {
        !self.webhook.is_empty()
    }

    /// Базовый URL вызова методов с учётом [`Self::base_url_override`].
    ///
    /// СОДЕРЖИТ СЕКРЕТ, если override пуст. `None` ⇒ вебхука нет или он
    /// нераспознаваем.
    pub fn api_base(&self) -> Option<String> {
        if !self.base_url_override.is_empty() {
            return Some(self.base_url_override.clone());
        }
        self.webhook.api_base()
    }

    /// Сохраняет вебхук в файл [`Self::config_path`] и в память.
    ///
    /// См. [`save_webhook_to`] — здесь только подстановка пути.
    ///
    /// # Errors
    ///
    /// Любая ошибка записи; целевой файл при этом остаётся нетронутым.
    pub fn save_webhook(&mut self, webhook: &WebhookUrl) -> anyhow::Result<()> {
        let path = self.config_path.clone();
        save_webhook_to(&path, webhook)?;
        self.webhook = webhook.clone();
        Ok(())
    }

    /// Стирает вебхук из файла [`Self::config_path`] и из памяти (logout).
    ///
    /// # Errors
    ///
    /// Ошибка чтения/записи конфига.
    pub fn clear_webhook(&mut self) -> anyhow::Result<()> {
        let path = self.config_path.clone();
        clear_webhook_to(&path)?;
        self.webhook = WebhookUrl::default();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Запись секрета на диск: единственный путь, которым вебхук покидает процесс.
// ---------------------------------------------------------------------------

/// Ключ вебхука в TOML-файле. Совпадает с именем поля [`BitrixConfig`]:
/// read-modify-write работает с сырой таблицей, и разъехавшееся имя тихо
/// создало бы ВТОРОЙ ключ вместо перезаписи первого.
pub const WEBHOOK_KEY: &str = "webhook";

/// Атомарно записывает вебхук в TOML-файл `path` с правами `0600`.
///
/// Инварианты (ради них функция и существует отдельно от `serde`) перечислены
/// в шапке модуля. Ключевые следствия:
///
/// - каталог (`~/.config/forge`) создаётся при необходимости;
/// - остальные ключи файла сохраняются как есть — пользователь мог задать
///   `poll_active_secs`, `cache_dir` и что угодно ещё;
/// - повторное сохранение ПЕРЕЗАПИСЫВАЕТ значение: мультипортальности в v1
///   нет (Р5), поле одно;
/// - в лог уходит только [`WebhookUrl::fingerprint`], никогда сам URL.
///
/// # Errors
///
/// Пустой вебхук (стирать нужно через [`clear_webhook_to`]); невозможность
/// создать каталог или файл; ошибка записи, `fsync` или `rename`.
pub fn save_webhook_to(path: &Path, webhook: &WebhookUrl) -> anyhow::Result<()> {
    if webhook.is_empty() {
        anyhow::bail!("the webhook must not be empty");
    }
    let mut doc = read_table(path)?;
    doc.insert(
        WEBHOOK_KEY.into(),
        // ЕДИНСТВЕННОЕ место во всём крейте, где секрет превращается в
        // сериализуемое значение.
        toml::Value::String(webhook.expose().to_string()),
    );
    write_table(path, &doc)?;
    tracing::info!(
        target: "forge_bitrix",
        path = %path.display(),
        webhook = %webhook.fingerprint(),
        "forge-bitrix: saved the webhook (only the fingerprint is ever logged)"
    );
    Ok(())
}

/// Убирает вебхук из TOML-файла `path` (logout).
///
/// Остальные ключи пользователя переживают операцию. Отсутствующий файл — НЕ
/// ошибка и не повод его создавать: выходить было не из чего, а состояние всё
/// равно обязано стать `not_configured`.
///
/// **На портал при этом не ходим.** Отозвать входящий вебхук через REST
/// невозможно — он остаётся действующим до тех пор, пока владелец не удалит
/// его в самом Битрикс24. Логика «сделаем вид, что отозвали» была бы враньём
/// в интерфейсе, поэтому UI говорит об этом прямо.
///
/// # Errors
///
/// Ошибка чтения/записи конфига.
pub fn clear_webhook_to(path: &Path) -> anyhow::Result<()> {
    let mut doc = read_table(path)?;
    if doc.remove(WEBHOOK_KEY).is_none() && !path.exists() {
        return Ok(());
    }
    write_table(path, &doc)?;
    tracing::info!(
        target: "forge_bitrix",
        path = %path.display(),
        "forge-bitrix: webhook removed from the config (the portal-side webhook is still valid)"
    );
    Ok(())
}

/// Read-modify-write, первая половина: читает TOML-таблицу целиком.
///
/// Битый файл НЕ роняет операцию: он переписывается с нуля с warn'ом. Иначе
/// одна опечатка в `bitrix.toml` навсегда лишала бы пользователя возможности
/// подключиться из UI — а починить её нечем, кроме текстового редактора.
fn read_table(path: &Path) -> anyhow::Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(toml::from_str(&body).unwrap_or_else(|e| {
            tracing::warn!(
                target: "forge_bitrix",
                path = %path.display(),
                error = %e,
                "forge-bitrix: bitrix.toml is not valid TOML; rewriting it from scratch"
            );
            toml::Table::new()
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(anyhow::anyhow!("failed to read {}: {e}", path.display())),
    }
}

/// Read-modify-write, вторая половина: атомарная запись таблицы с `0600`.
fn write_table(path: &Path, doc: &toml::Table) -> anyhow::Result<()> {
    let body = toml::to_string_pretty(doc)
        .map_err(|e| anyhow::anyhow!("failed to serialize bitrix.toml: {e}"))?;

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
///
/// Временный файл кладётся В ТОТ ЖЕ каталог намеренно: `rename` атомарен
/// только внутри одной файловой системы, а `/tmp` на многих машинах — другая.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bitrix.toml".to_string());
    let tmp_name = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// Создаёт файл с правами `0600` ДО записи содержимого, пишет и `fsync`'ит.
///
/// `set_owner_only` вызывается дополнительно и ДО первого `write_all`: файл
/// мог остаться от упавшего процесса (открываем `create`, а не `create_new`),
/// и тогда `mode()` из `OpenOptions` не применяется вовсе.
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
    set_owner_only(path)?;
    f.write_all(bytes)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    // Без fsync переименование может опередить сброс содержимого на диск: при
    // отключении питания остался бы файл нулевой длины ВМЕСТО старого.
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
/// Вызывается на КАЖДОЙ загрузке, а не только при записи (см. шапку модуля).
/// Возвращает `true`, если права были изменены — это нужно тесту, а не
/// вызывающему коду.
pub(crate) fn tighten_perms_if_needed(path: &Path) -> bool {
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
                    target: "forge_bitrix",
                    path = %path.display(),
                    mode = format!("{mode:o}"),
                    "forge-bitrix: bitrix.toml was readable by others; tightened to 0600"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    error = %e,
                    "forge-bitrix: could not tighten bitrix.toml permissions"
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

fn apply_env_u64(key: &str, target: &mut u64) {
    if let Ok(v) = std::env::var(key) {
        if v.is_empty() {
            return;
        }
        match v.parse::<u64>() {
            Ok(n) => *target = n,
            Err(e) => tracing::warn!(
                target: "forge_bitrix",
                key = %key,
                value = %v,
                error = %e,
                "forge-bitrix: env value is not a u64; ignored"
            ),
        }
    }
}

fn apply_env_u32(key: &str, target: &mut u32) {
    if let Ok(v) = std::env::var(key) {
        if v.is_empty() {
            return;
        }
        match v.parse::<u32>() {
            Ok(n) => *target = n,
            Err(e) => tracing::warn!(
                target: "forge_bitrix",
                key = %key,
                value = %v,
                error = %e,
                "forge-bitrix: env value is not a u32; ignored"
            ),
        }
    }
}

fn fix_u64(name: &str, target: &mut u64, min: u64, max: u64, fallback: u64) {
    if *target < min || *target > max {
        tracing::warn!(
            target: "forge_bitrix",
            field = %name,
            value = *target,
            "forge-bitrix: value out of range; falling back to the default"
        );
        *target = fallback;
    }
}

/// Раскрывает ведущий `~` в пути (TOML не умеет этого сам).
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig::load_from_file(&dir.path().join("nope.toml"));
        assert!(cfg.webhook.is_empty());
        assert_eq!(cfg.poll_active_secs, DEFAULT_POLL_ACTIVE_SECS);
        assert_eq!(cfg.poll_idle_secs, DEFAULT_POLL_IDLE_SECS);
        assert_eq!(cfg.recent_sync_secs, DEFAULT_RECENT_SYNC_SECS);
        assert_eq!(cfg.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(cfg.rate_limit_rps, DEFAULT_RATE_LIMIT_RPS);
        assert_eq!(cfg.burst, DEFAULT_BURST);
    }

    #[test]
    fn broken_toml_yields_full_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(&path, "this is ] not [ toml").unwrap();
        let cfg = BitrixConfig::load_from_file(&path);
        assert!(cfg.webhook.is_empty());
        assert_eq!(cfg.poll_active_secs, DEFAULT_POLL_ACTIVE_SECS);
    }

    /// Интервалы обязаны читаться из файла: тесты Ф7 инжектируют короткие
    /// значения вместо боевых секунд.
    #[test]
    fn intervals_come_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "poll_active_secs = 1\npoll_idle_secs = 2\nrecent_sync_secs = 3\nrate_limit_rps = 1\nburst = 2\n",
        )
        .unwrap();
        let cfg = BitrixConfig::load_from_file(&path);
        assert_eq!(cfg.poll_active_secs, 1);
        assert_eq!(cfg.poll_idle_secs, 2);
        assert_eq!(cfg.recent_sync_secs, 3);
        assert_eq!(cfg.rate_limit_rps, 1);
        assert_eq!(cfg.burst, 2);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(&path, "poll_active_secs = 9\n").unwrap();
        let cfg = BitrixConfig::load_from_file(&path);
        assert_eq!(cfg.poll_active_secs, 9);
        assert_eq!(cfg.poll_idle_secs, DEFAULT_POLL_IDLE_SECS);
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ: `Debug` секрета не раскрывает ни кода, ни хоста.
    #[test]
    fn debug_never_reveals_the_webhook() {
        let w = WebhookUrl::new("https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/");
        assert_eq!(format!("{w:?}"), "<redacted>");
        let cfg = BitrixConfig {
            webhook: w,
            ..BitrixConfig::default()
        };
        let dump = format!("{cfg:?}");
        assert!(!dump.contains("wqdzbfhmn83d23j4"), "{dump}");
        assert!(dump.contains("<redacted>"), "{dump}");
    }

    #[test]
    fn fingerprint_hides_the_code_but_identifies_the_portal() {
        let w = WebhookUrl::new("https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/");
        let fp = w.fingerprint();
        assert_eq!(fp, "test-work-2.bitrix24.ru#1…3d23j4");
        assert!(!fp.contains("wqdzbfh"), "{fp}");
        assert_eq!(WebhookUrl::default().fingerprint(), "<none>");
        assert_eq!(WebhookUrl::new("garbage").fingerprint(), "<malformed>");
    }

    #[test]
    fn parts_accepts_both_trailing_slash_forms() {
        let a = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");
        let b = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123");
        assert_eq!(a.parts(), Some(("https://p.bitrix24.ru", "1", "abc123")));
        assert_eq!(a.parts(), b.parts());
        assert_eq!(
            a.api_base().as_deref(),
            Some("https://p.bitrix24.ru/rest/1/abc123")
        );
        assert_eq!(a.portal_host().as_deref(), Some("p.bitrix24.ru"));
    }

    #[test]
    fn parts_rejects_malformed_input() {
        for raw in [
            "",
            "garbage",
            "ftp://p.ru/rest/1/abc",
            "https:///rest/1/abc",
            "https://p.ru/rest/abc/1",
            "https://p.ru/rest/1/ab-c",
            "https://p.ru/rest/1/abc/profile",
            "https://p.ru/1/abc",
        ] {
            assert!(
                WebhookUrl::new(raw).parts().is_none(),
                "принят мусорный вход: {raw:?}"
            );
        }
    }

    #[test]
    fn validate_rejects_out_of_range_values() {
        let base = BitrixConfig::default();
        assert!(base.validate().is_ok());

        let mut c = base.clone();
        c.rate_limit_rps = 0;
        assert!(c.validate().is_err());

        let mut c = base.clone();
        c.rate_limit_rps = MAX_RATE_LIMIT_RPS + 1;
        assert!(c.validate().is_err());

        let mut c = base.clone();
        c.max_upload_bytes = 0;
        assert!(c.validate().is_err());

        let mut c = base.clone();
        c.poll_active_secs = 0;
        assert!(c.validate().is_err());

        let mut c = base;
        c.burst = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_and_fix_repairs_instead_of_failing() {
        let mut c = BitrixConfig {
            poll_active_secs: 0,
            rate_limit_rps: 999,
            burst: 0,
            max_upload_bytes: 0,
            connect_timeout_secs: 0,
            request_timeout_secs: MAX_TIMEOUT_SECS + 1,
            base_url_override: "  https://stub.example/rest/1/abc123/  ".into(),
            ..BitrixConfig::default()
        };
        c.validate_and_fix();
        assert!(c.validate().is_ok());
        assert_eq!(c.poll_active_secs, DEFAULT_POLL_ACTIVE_SECS);
        assert_eq!(c.rate_limit_rps, DEFAULT_RATE_LIMIT_RPS);
        assert_eq!(c.burst, DEFAULT_BURST);
        assert_eq!(c.max_upload_bytes, DEFAULT_MAX_UPLOAD_BYTES);
        assert_eq!(c.connect_timeout_secs, DEFAULT_CONNECT_TIMEOUT_SECS);
        assert_eq!(c.request_timeout_secs, DEFAULT_REQUEST_TIMEOUT_SECS);
        // Пробелы и хвостовой слэш срезаны, схема https принята как есть.
        assert_eq!(c.base_url_override, "https://stub.example/rest/1/abc123");
    }

    /// ПРАВИЛО Р17 целиком, обеими ветками, без подмены переменных процесса:
    /// признак изолированного окружения передаётся параметром.
    #[test]
    fn r17_allows_https_everywhere_and_plain_http_only_on_loopback_in_tests() {
        // https — всегда законен.
        for url in [
            "https://test-work-2.bitrix24.ru/rest/1/abc123",
            "https://127.0.0.1:8443/rest/1/abc123",
        ] {
            assert!(check_base_url_override(url, false).is_ok(), "{url}");
            assert!(check_base_url_override(url, true).is_ok(), "{url}");
        }

        // Пустое значение — «переопределения нет», а не ошибка.
        assert!(check_base_url_override("", false).is_ok());
        assert!(check_base_url_override("   ", false).is_ok());

        // http на петлевой хост: только в изолированном окружении.
        for url in [
            "http://127.0.0.1:53219/rest/1/abc123",
            "http://localhost:53219",
            "http://[::1]:53219/rest/1/abc123",
        ] {
            assert!(
                check_base_url_override(url, false).is_err(),
                "http без FORGE_BITRIX_CONFIG_PATH обязан отвергаться: {url}"
            );
            assert!(check_base_url_override(url, true).is_ok(), "{url}");
        }

        // Чужой хост по http — отвергается ВСЕГДА, тестовый режим не помогает.
        for url in [
            "http://example.com/rest/1/abc123",
            "http://192.168.0.10/rest/1/abc123",
            "http://evil.test:80",
            "ftp://p.ru/rest/1/abc123",
            "just-a-string",
        ] {
            assert!(check_base_url_override(url, false).is_err(), "{url}");
            assert!(
                check_base_url_override(url, true).is_err(),
                "тестовый режим не должен открывать чужие хосты: {url}"
            );
        }
    }

    /// Текст ошибки Р17 НЕ цитирует само значение: в `base_url_override`
    /// может лежать полный путь вебхука вместе с кодом.
    #[test]
    fn r17_error_text_never_quotes_the_value() {
        let err = check_base_url_override("http://evil.example/rest/1/wqdzbfhmn83d23j4", false)
            .unwrap_err();
        assert!(!err.contains("wqdzbfhmn83d23j4"), "{err}");
        assert!(!err.contains("evil.example"), "{err}");
    }

    /// Та же ветка, но через реальную переменную окружения: `validate`
    /// обязана смотреть именно на `FORGE_BITRIX_CONFIG_PATH`.
    #[test]
    fn r17_reads_the_isolation_flag_from_the_environment() {
        // Переменная процесса — глобальный ресурс; тесты крейта идут в
        // потоках одного процесса, поэтому ветка сериализована мьютексом, а
        // прежнее значение восстанавливается.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let previous = std::env::var_os("FORGE_BITRIX_CONFIG_PATH");
        let cfg = BitrixConfig {
            base_url_override: "http://127.0.0.1:53219/rest/1/abc123".into(),
            ..BitrixConfig::default()
        };

        std::env::remove_var("FORGE_BITRIX_CONFIG_PATH");
        assert!(!is_isolated_env());
        assert!(
            cfg.validate().is_err(),
            "в боевом режиме http://127.0.0.1 обязан отвергаться"
        );

        std::env::set_var("FORGE_BITRIX_CONFIG_PATH", "/tmp/forge-bitrix-test.toml");
        assert!(is_isolated_env());
        assert!(
            cfg.validate().is_ok(),
            "в изолированном режиме стенд обязан работать"
        );

        match previous {
            Some(v) => std::env::set_var("FORGE_BITRIX_CONFIG_PATH", v),
            None => std::env::remove_var("FORGE_BITRIX_CONFIG_PATH"),
        }
    }

    /// Таймауты — данные конфига: без них зависший портал держал бы вызов
    /// вечно (превышение лимита интенсивности выглядит именно так).
    #[test]
    fn timeouts_come_from_the_file_and_are_range_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "connect_timeout_secs = 3\nrequest_timeout_secs = 7\n",
        )
        .unwrap();
        let cfg = BitrixConfig::load_from_file(&path);
        assert_eq!(cfg.connect_timeout_secs, 3);
        assert_eq!(cfg.request_timeout_secs, 7);
        assert!(cfg.validate().is_ok());

        let mut c = BitrixConfig {
            request_timeout_secs: 0,
            ..BitrixConfig::default()
        };
        assert!(c.validate().is_err());
        c.request_timeout_secs = MAX_TIMEOUT_SECS + 1;
        assert!(c.validate().is_err());
    }

    /// Пустой вебхук — не ошибка: «включён, но ещё не подключён» это штатное
    /// стартовое состояние плагина.
    #[test]
    fn empty_webhook_is_valid() {
        let c = BitrixConfig::default();
        assert!(!c.has_webhook());
        assert!(c.validate().is_ok());
        assert!(c.api_base().is_none());
    }

    // -------------------------------------------------------------------
    // Запись секрета на диск (Ф3).
    // -------------------------------------------------------------------

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ ЗАПИСИ: файл создаётся сразу с правами `0600`.
    ///
    /// Проверяется и первая запись (файла не было), и повторная (файл уже
    /// лежал с широкими правами): во втором случае `OpenOptions::mode` не
    /// применяется вовсе, и права держит только явный `chmod` до `write_all`.
    #[cfg(unix)]
    #[test]
    fn the_config_file_is_created_with_owner_only_permissions() {
        let dir = tempfile::tempdir().unwrap();
        // Вложенный каталог: `~/.config/forge` может не существовать вовсе.
        let path = dir.path().join("config/forge/bitrix.toml");
        let w = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");

        save_webhook_to(&path, &w).unwrap();
        assert_eq!(mode_of(&path), 0o600, "секрет обязан лежать 0600");

        // Файл «испортили» снаружи — повторная запись обязана вернуть 0600.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_webhook_to(&path, &WebhookUrl::new("https://p.bitrix24.ru/rest/1/zzz999zzz999/"))
            .unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    /// Права чинятся и на ЧТЕНИИ: пользователь мог создать файл руками.
    #[cfg(unix)]
    #[test]
    fn loading_tightens_permissions_of_a_hand_made_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(&path, "webhook = \"https://p.bitrix24.ru/rest/1/abc123/\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let cfg = BitrixConfig::load_from_file(&path);
        assert!(cfg.has_webhook());
        assert_eq!(mode_of(&path), 0o600, "загрузка обязана сузить права");
        // Второй раз чинить уже нечего.
        assert!(!tighten_perms_if_needed(&path));
    }

    /// Запись атомарна и не оставляет мусора: после успеха в каталоге лежит
    /// РОВНО целевой файл, временного нет.
    #[test]
    fn the_write_is_atomic_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let w = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");

        save_webhook_to(&path, &w).unwrap();
        let tmp = tmp_path_for(&path);
        assert!(!tmp.exists(), "временный файл не убран: {}", tmp.display());

        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["bitrix.toml".to_string()], "{entries:?}");

        // Файл читается обратно целиком — то есть он не обрезан.
        let back = BitrixConfig::load_from_file(&path);
        assert_eq!(back.webhook.expose(), w.expose());
    }

    /// Прерывание между записью и `rename` не оставляет повреждённого
    /// конфига: старое содержимое остаётся ровно таким, каким было, а
    /// недописанный tmp — это отдельный файл, который никто не читает.
    #[test]
    fn a_crash_between_write_and_rename_keeps_the_old_config_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        save_webhook_to(&path, &WebhookUrl::new("https://p.bitrix24.ru/rest/1/oldcode1234/")).unwrap();

        // Имитируем упавший на середине процесс: tmp написан, rename не
        // случился.
        let tmp = tmp_path_for(&path);
        write_secret_file(&tmp, b"webhook = \"https://p.bitrix24.ru/rest/1/hal").unwrap();

        let cfg = BitrixConfig::load_from_file(&path);
        assert_eq!(
            cfg.webhook.expose(),
            "https://p.bitrix24.ru/rest/1/oldcode1234/",
            "целевой файл обязан остаться прежним"
        );

        // Следующая успешная запись переиспользует tmp и доводит дело до конца.
        save_webhook_to(&path, &WebhookUrl::new("https://p.bitrix24.ru/rest/1/newcode1234/")).unwrap();
        assert!(!tmp.exists());
        assert_eq!(
            BitrixConfig::load_from_file(&path).webhook.expose(),
            "https://p.bitrix24.ru/rest/1/newcode1234/"
        );
    }

    /// Read-modify-write: чужие ключи пользователя переживают и сохранение, и
    /// стирание вебхука.
    #[test]
    fn other_toml_keys_survive_save_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "poll_active_secs = 7\nsomething_from_the_future = \"keep me\"\n",
        )
        .unwrap();

        save_webhook_to(&path, &WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/")).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("poll_active_secs = 7"), "{body}");
        assert!(body.contains("something_from_the_future"), "{body}");
        assert_eq!(BitrixConfig::load_from_file(&path).poll_active_secs, 7);

        clear_webhook_to(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("rest/1/abc123"), "{body}");
        assert!(body.contains("poll_active_secs = 7"), "{body}");
        assert!(body.contains("something_from_the_future"), "{body}");
        assert!(!BitrixConfig::load_from_file(&path).has_webhook());
    }

    /// Повторное сохранение ПЕРЕЗАПИСЫВАЕТ: мультипортальности в v1 нет (Р5),
    /// второго ключа появиться не должно.
    #[test]
    fn saving_twice_overwrites_instead_of_appending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let mut cfg = BitrixConfig {
            config_path: path.clone(),
            ..BitrixConfig::default()
        };

        cfg.save_webhook(&WebhookUrl::new("https://a.bitrix24.ru/rest/1/aaa111aaa111/"))
            .unwrap();
        cfg.save_webhook(&WebhookUrl::new("https://b.bitrix24.ru/rest/2/bbb222bbb222/"))
            .unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.matches("webhook =").count(), 1, "{body}");
        assert!(!body.contains("aaa111aaa111"), "{body}");
        // Память обновлена вместе с диском.
        assert_eq!(cfg.webhook.portal_host().as_deref(), Some("b.bitrix24.ru"));

        cfg.clear_webhook().unwrap();
        assert!(!cfg.has_webhook());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("bbb222"));
    }

    /// Битый TOML не запирает пользователя: файл переписывается с нуля, и
    /// подключиться из UI по-прежнему можно.
    #[test]
    fn a_broken_toml_is_rewritten_instead_of_failing_the_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(&path, "this is ] not [ toml").unwrap();

        save_webhook_to(&path, &WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/")).unwrap();
        assert!(BitrixConfig::load_from_file(&path).has_webhook());
    }

    /// Logout по несуществующему файлу — не ошибка и НЕ повод его создавать.
    #[test]
    fn clearing_a_missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        clear_webhook_to(&path).unwrap();
        assert!(!path.exists(), "пустой конфиг не должен появляться из ниоткуда");
    }

    /// Пустой вебхук не сохраняется: стирание — отдельная операция, и
    /// подменять её записью пустой строки значит потерять различие «logout» и
    /// «сохранили мусор».
    #[test]
    fn saving_an_empty_webhook_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        assert!(save_webhook_to(&path, &WebhookUrl::default()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn base_url_override_wins_over_the_webhook() {
        let c = BitrixConfig {
            webhook: WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/"),
            base_url_override: "http://127.0.0.1:9/rest/1/stub".into(),
            ..BitrixConfig::default()
        };
        assert_eq!(c.api_base().as_deref(), Some("http://127.0.0.1:9/rest/1/stub"));
    }
}
