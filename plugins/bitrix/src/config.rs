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
//! Ровно одним путём — [`save_portals_to`], и у него четыре инварианта:
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
//! # Мультипортальность: `[[portals]]`
//!
//! Файл описывает N подключений массивом таблиц:
//!
//! ```toml
//! poll_active_secs = 4          # общие настройки — один раз на машину
//! cache_dir = "~/.forge/bitrix"
//!
//! [[portals]]
//! id = "acme"                   # выводится из ХОСТА, секрета не содержит
//! title = "Acme"                # необязательно
//! webhook = "https://acme.bitrix24.ru/rest/1/…/"
//!
//! [[portals]]
//! id = "beta"
//! webhook = "https://beta.bitrix24.ru/rest/9/…/"
//! ```
//!
//! Разделение уровней принципиально: корневые ключи — настройки МАШИНЫ,
//! `[[portals]]` — список подключений. [`BitrixRootConfig::portal_configs`]
//! раздаёт общие настройки КОПИЕЙ в пер-портальные [`BitrixConfig`], поэтому
//! `B24State::new(cfg)` и весь код под ним о мультипортальности не знают.
//!
//! **Совместимость.** Верхнеуровневый ключ `webhook` остаётся ЧИТАЕМЫМ: файл
//! без `[[portals]]` даёт ровно один портал с идентификатором, выведенным из
//! хоста. Миграция происходит при первой ЗАПИСИ ([`save_portals_to`] снимает
//! верхнеуровневый ключ), а не при чтении: однократный диагностический запуск
//! не должен переписывать чужой файл. Идентификатор портала-мигранта
//! детерминирован — на него завязаны подкаталоги кэша и пер-портальные
//! настройки.
//!
//! # «Анализ чата через Claude»
//!
//! Пять полей фичи живут здесь же: [`BitrixConfig::prompts_path`],
//! [`BitrixConfig::claude_cli_path`], [`BitrixConfig::analysis_timeout_secs`],
//! [`BitrixConfig::analysis_max_chars`], [`BitrixConfig::analysis_model`],
//! [`BitrixConfig::claude_enabled`]. Значения по умолчанию совпадают с VK и
//! Telegram: это одна фича на три мессенджера.
//!
//! Env-override: `FORGE_BITRIX_PROMPTS_PATH`,
//! `FORGE_BITRIX_CLAUDE_CLI_PATH`, `FORGE_BITRIX_ANALYSIS_TIMEOUT_SECS`,
//! `FORGE_BITRIX_ANALYSIS_MAX_CHARS`, `FORGE_BITRIX_ANALYSIS_MODEL`,
//! `FORGE_BITRIX_CLAUDE_ENABLED` (плюс глобальный `FORGE_CLAUDE_RUNNER_ENABLED`,
//! включающий LLM-запуски во всех плагинах разом),
//! `FORGE_BITRIX_LLM_BACKEND` (`claude`|`kimi`|`cloud`),
//! `FORGE_BITRIX_KIMI_CLI_PATH`, `FORGE_BITRIX_LLM_CLOUD_BASE_URL`,
//! `FORGE_BITRIX_LLM_CLOUD_API_KEY`, `FORGE_BITRIX_LLM_CLOUD_MODEL`.
//! Первый обязателен для герметичности `cargo test`: без него любой тест,
//! дёрнувший сохранение заметки, затирал бы живой
//! `~/.forge/bitrix-chat-prompts.json` разработчика.
//!
//! Хранилище заметок пишет `forge_chat_analysis::prompts::save` через ОБЩУЮ
//! `atomic::write_bytes_atomic` — тот же алгоритм, что у [`write_table`], но
//! ровно в одном экземпляре на три плагина, а не третьей копией здесь.
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

// ---------------------------------------------------------------------------
// «Анализ чата через Claude».
//
// Значения совпадают с VK и Telegram (`plugins/vk/src/config.rs`,
// `plugins/telegram/src/config.rs`) намеренно: это ОДНА фича на три
// мессенджера, и разные умолчания означали бы, что в одной вкладке разбор
// укладывается в бюджет, а в соседней — нет, без единого сообщения о причине.
// ---------------------------------------------------------------------------

/// Таймаут одного анализа чата, секунды. `0` ⇒ таймаут выключен.
///
/// Значение совпадает с [`forge_claude_runner::DEFAULT_RUN_TIMEOUT_SECS`] и
/// продублировано здесь намеренно: это ДЕФОЛТ КОНФИГА плагина, а раннер живёт
/// в чужом крейте и о `bitrix.toml` ничего не знает.
pub const DEFAULT_ANALYSIS_TIMEOUT_SECS: u64 = 300;

/// Потолок транскрипта, отдаваемого модели, в CODE POINTS.
///
/// Совпадает с [`crate::analysis::context::MAX_TRANSCRIPT_CHARS`]: константа
/// задаёт величину, конфиг — возможность её подвинуть на конкретной машине.
pub const DEFAULT_ANALYSIS_MAX_CHARS: usize = 60_000;

/// Нижняя граница `analysis_max_chars`. Меньше 5 000 символов транскрипт
/// перестаёт быть перепиской: в него влезает десяток реплик, и любой ответ
/// модели будет выводом «данных не хватает».
pub const MIN_ANALYSIS_MAX_CHARS: usize = 5_000;

/// Верхняя граница `analysis_max_chars`. 200 000 code points — это уже около
/// 50 тысяч токенов на один запрос; выше значение означает опечатку, а не
/// намерение.
pub const MAX_ANALYSIS_MAX_CHARS: usize = 200_000;

/// Модель Claude для анализа чата по умолчанию.
///
/// `sonnet`, а не `opus`: анализ запускается кнопкой и должен стоить дёшево;
/// значение — зафиксированное решение, а не подбор.
pub const DEFAULT_ANALYSIS_MODEL: &str = "sonnet";

/// Дефолтный путь хранилища пер-чатовых промптов
/// (`~/.forge/bitrix-chat-prompts.json`).
///
/// Рядом с кэшем медиа, а НЕ рядом с `bitrix.toml`: это пользовательские
/// данные вкладки, а не настройки, и переносить их вместе с конфигом (в
/// котором лежит СЕКРЕТ вебхука) незачем. Заодно это разводит два файла с
/// разной ценой утечки: заметки о собеседниках приватны, но вебхук — это
/// доступ к порталу целиком.
pub fn default_prompts_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".forge/bitrix-chat-prompts.json")
    } else {
        PathBuf::from("./bitrix-chat-prompts.json")
    }
}

/// Фактический путь хранилища промптов: непустой `FORGE_BITRIX_PROMPTS_PATH`
/// побеждает, иначе — [`default_prompts_path`].
///
/// Существует по той же причине, что и [`config_path`], и это не
/// теоретическая аккуратность: `forge_chat_analysis::prompts::save` ПИШЕТ по
/// этому пути, и без override любой `cargo test`, дёрнувший сохранение,
/// затирал бы живые заметки разработчика в `~/.forge`.
pub fn prompts_path() -> PathBuf {
    match std::env::var_os("FORGE_BITRIX_PROMPTS_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_prompts_path(),
    }
}

/// Путь файла пользовательских настроек хоста
/// (`~/.forge/user_settings.json`).
///
/// Строится ядром ([`forge_voice::prefs::default_path`]), а не здесь: тот же
/// путь строит хост в `tmux-web/src/main.rs`, и третье написание означало бы,
/// что плагин читает файл, которого никто не пишет.
pub fn user_settings_path() -> PathBuf {
    forge_voice::prefs::default_path()
}

/// Дефолтный путь бинаря Claude CLI (`$HOME/.local/bin/claude`).
///
/// Тот же дефолт, что у `forge_echo::config`, `forge_vk::config` и
/// `forge_telegram::config`: все они запускают ОДИН И ТОТ ЖЕ CLI, и разные
/// умолчания означали бы, что чат Echo работает, а анализ чата Битрикса — нет,
/// без единого сообщения о причине.
pub fn default_claude_cli_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/bin/claude")
    } else {
        PathBuf::from("claude")
    }
}

fn default_analysis_timeout_secs() -> u64 {
    DEFAULT_ANALYSIS_TIMEOUT_SECS
}

fn default_analysis_max_chars() -> usize {
    DEFAULT_ANALYSIS_MAX_CHARS
}

fn default_analysis_model() -> Option<String> {
    Some(DEFAULT_ANALYSIS_MODEL.to_string())
}

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

// ---------------------------------------------------------------------------
// Мультипортальность: `[[portals]]` и вывод идентификатора портала.
// ---------------------------------------------------------------------------

/// Идентификатор портала, когда вывести его из вебхука не удалось.
///
/// Уникальность всё равно обеспечивает [`unique_portal_id`], поэтому два
/// нераспознанных вебхука дадут `portal` и `portal-2`, а не молча сольются.
pub const FALLBACK_PORTAL_ID: &str = "portal";

/// Одна запись `[[portals]]` в `bitrix.toml`.
///
/// Три поля и ни одного лишнего:
///
/// - `id` — стабильный идентификатор портала, выведенный из ХОСТА вебхука
///   ([`portal_id_from_webhook`]). Секрета не содержит: его можно отдавать во
///   фронт, класть в WS-кадры и писать в логи. На этом свойстве держатся
///   заголовок `X-Forge-Portal`, пер-портальные подкаталоги кэша и
///   пер-портальные настройки;
/// - `title` — необязательное человеческое имя вкладки. Пусто ⇒ фронт
///   показывает хост портала;
/// - `webhook` — СЕКРЕТ целиком, тип [`WebhookUrl`] (без `Serialize`).
///
/// `id` в файле необязателен: отсутствующий или пустой выводится из вебхука
/// при загрузке, а на диск попадает уже явным — чтобы правка вебхука руками
/// не переименовала портал и не отвязала от него кэш и настройки.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PortalConfig {
    /// Стабильный идентификатор портала. Пусто ⇒ выводится из вебхука.
    pub id: String,

    /// Отображаемое имя. Пусто ⇒ показывается хост портала.
    pub title: String,

    /// URL входящего вебхука — секрет целиком.
    pub webhook: WebhookUrl,
}

impl PortalConfig {
    /// Портал с идентификатором, выведенным из вебхука.
    pub fn from_webhook(webhook: &WebhookUrl, title: Option<&str>) -> Self {
        Self {
            id: portal_id_from_webhook(webhook),
            title: title.unwrap_or_default().trim().to_string(),
            webhook: webhook.clone(),
        }
    }

    /// Имя для интерфейса: `title`, иначе хост, иначе идентификатор.
    ///
    /// Секрета не содержит ни в одной из трёх веток: хост — единственная часть
    /// вебхука, которую можно показывать (см. [`WebhookUrl::portal_host`]).
    ///
    /// **В HTTP-ответ не годится.** Хост портала границу HTTP не пересекает
    /// вовсе — ни `GET /api/bitrix/status`, ни `GET /api/bitrix/portals` его не
    /// отдают (шапка `crate::routes::portals`). Подпись вкладки во фронте
    /// собирается из `title` с фолбэком на `id`; эта функция — для лога и
    /// серверных текстов.
    pub fn display_name(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        self.webhook
            .portal_host()
            .unwrap_or_else(|| self.id.clone())
    }
}

/// Приводит произвольную строку к виду идентификатора: нижний регистр,
/// только `[a-z0-9-]`, без сдвоенных и краевых дефисов.
///
/// Всё, что не ASCII-буквоцифра (включая кириллицу IDN-домена), становится
/// дефисом, поэтому результат может оказаться ПУСТЫМ — вызывающий обязан
/// подставить фолбэк.
pub fn normalize_portal_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Имя ПОДКАТАЛОГА (или суффикса файла), которым портал адресует свои данные
/// на диске (Ф6).
///
/// # Почему это не просто `portal_id`
///
/// Идентификатор портала попадает в путь на файловой системе, а значит это
/// источник path traversal: `id`, равный `..`, увёл бы кэш на уровень выше
/// общего каталога, а `a/b` — в чужой подкаталог. [`normalize_portal_id`] уже
/// оставляет только `[a-z0-9-]`, и после неё ни разделителя, ни `.`, ни `..`
/// в строке не остаётся ФИЗИЧЕСКИ. Но полагаться на это молча нельзя: правка
/// нормализации (например, «разрешим точку в имени портала») тихо превратила
/// бы её в дыру за один коммит. Поэтому компонент нормализуется ЗАНОВО прямо
/// здесь, и результат ещё раз проверяется на разделители и на `.`/`..`.
///
/// # Пустой идентификатор
///
/// Слот без портала (свежая установка, экран подключения) идентификатора не
/// имеет — ему достаётся [`crate::ws::VACANT_PORTAL_ID`]. Значение
/// зарезервировано: у настоящего портала `id` начинаться с подчёркивания не
/// может, потому что [`normalize_portal_id`] подчёркивание вырезает. Писать
/// туда практически нечего (без вебхука портал не ходит в сеть), но каталог
/// обязан быть ОПРЕДЕЛЁН: иначе пустой слот писал бы в общий корень, вперемешку
/// с подкаталогами настоящих порталов.
#[must_use]
pub fn portal_dir_component(portal_id: &str) -> String {
    let safe = normalize_portal_id(portal_id);
    let rejected = safe.is_empty()
        || safe == "."
        || safe == ".."
        || safe.contains(std::path::MAIN_SEPARATOR)
        || safe.contains('/')
        || safe.contains('\\')
        || safe.contains('\0');
    if rejected {
        return crate::ws::VACANT_PORTAL_ID.to_string();
    }
    safe
}

/// Каталог данных ОДНОГО портала внутри общего `cache_dir`.
///
/// `~/.forge/bitrix-cache` + `acme` ⇒ `~/.forge/bitrix-cache/acme`. Внутри него
/// лежит ровно то же, что раньше лежало в корне: `media/`, `uploads/`,
/// `events.json`, `voice/`.
#[must_use]
pub fn portal_cache_dir(cache_root: &Path, portal_id: &str) -> PathBuf {
    cache_root.join(portal_dir_component(portal_id))
}

/// Файл заметок и разборов ОДНОГО портала: `<стем>-<portal>.<расширение>`.
///
/// `~/.forge/bitrix-chat-prompts.json` + `acme` ⇒
/// `~/.forge/bitrix-chat-prompts-acme.json`.
///
/// Суффикс, а не подкаталог: базовый путь задаёт пользователь (или
/// `FORGE_BITRIX_PROMPTS_PATH`), и он вправе указать конкретный ФАЙЛ. Превратив
/// его в каталог, мы бы сломали и то, и другое ожидание сразу.
#[must_use]
pub fn portal_prompts_path(prompts_root: &Path, portal_id: &str) -> PathBuf {
    let component = portal_dir_component(portal_id);
    let stem = prompts_root
        .file_stem()
        .map_or_else(|| "bitrix-chat-prompts".to_string(), |s| s.to_string_lossy().into_owned());
    let name = match prompts_root.extension() {
        Some(ext) => format!("{stem}-{component}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{component}"),
    };
    prompts_root.with_file_name(name)
}

/// FNV-1a (32 бита от 64-битного состояния) в hex — детерминированный на все
/// времена фолбэк-суффикс.
///
/// Намеренно НЕ `DefaultHasher`: у того нет гарантии стабильности между
/// версиями Rust, а идентификатор портала обязан пережить обновление
/// тулчейна — на него завязаны каталоги кэша и пер-портальные настройки.
fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", ((h >> 32) ^ h) as u32)
}

/// Выводит идентификатор портала из ХОСТА вебхука.
///
/// `https://test-work-2.bitrix24.ru/rest/1/abcdef/` ⇒ `test-work-2`.
///
/// **Из пути вебхука не берётся ничего**: ни `user_id`, ни код. Это ключевое
/// свойство — идентификатор уезжает во фронт и в логи.
///
/// Нераспознанный вебхук даёт [`FALLBACK_PORTAL_ID`]: где в такой строке код,
/// мы не знаем, и хэшировать её (а значит, публиковать функцию от секрета)
/// незачем.
pub fn portal_id_from_webhook(webhook: &WebhookUrl) -> String {
    match webhook.portal_host() {
        Some(host) => portal_id_from_host(&host),
        None => FALLBACK_PORTAL_ID.to_string(),
    }
}

/// Идентификатор из голого хоста (порт отбрасывается).
///
/// Обычный домен ⇒ первая метка (`test.bitrix24.ru` ⇒ `test`): именно её
/// пользователь называет «своим порталом». IP-литерал ⇒ хост целиком
/// (`192.168.1.5` ⇒ `192-168-1-5`), иначе стенды на `127.0.0.1:8081` и
/// `127.0.0.1:8082` слились бы в `127`.
pub fn portal_id_from_host(host: &str) -> String {
    let host = host.trim();
    if host.is_empty() {
        return FALLBACK_PORTAL_ID.to_string();
    }

    // `[::1]:8080` ⇒ `::1`; `example.com:8443` ⇒ `example.com`; голый IPv6
    // без скобок остаётся как есть (двоеточий больше одного).
    let bare = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        match host.rsplit_once(':') {
            Some((h, port))
                if !h.is_empty()
                    && !h.contains(':')
                    && !port.is_empty()
                    && port.bytes().all(|b| b.is_ascii_digit()) =>
            {
                h
            }
            _ => host,
        }
    };

    let is_literal =
        bare.contains(':') || (!bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
    let candidate = if is_literal {
        bare
    } else {
        bare.split('.').next().unwrap_or(bare)
    };

    let id = normalize_portal_id(candidate);
    if !id.is_empty() {
        return id;
    }
    // Первая метка не дала ничего печатного (например, IDN-домен): пробуем
    // хост целиком, и только потом — детерминированный хэш ХОСТА.
    let whole = normalize_portal_id(bare);
    if !whole.is_empty() {
        return whole;
    }
    format!("{FALLBACK_PORTAL_ID}-{}", fnv1a_hex(bare))
}

/// Разрешает коллизию идентификаторов суффиксом `-2`, `-3`, …
///
/// Уже занятые идентификаторы НЕ меняются: два вебхука одного портала (разные
/// `user_id`) дают `acme` и `acme-2`, и первый портал не переезжает — иначе
/// вместе с ним переехали бы его кэш и настройки.
pub fn unique_portal_id(base: &str, taken: &[String]) -> String {
    let base = if base.is_empty() {
        FALLBACK_PORTAL_ID.to_string()
    } else {
        base.to_string()
    };
    if !taken.iter().any(|t| t == &base) {
        return base;
    }
    for n in 2u32..=10_000 {
        let candidate = format!("{base}-{n}");
        if !taken.iter().any(|t| t == &candidate) {
            return candidate;
        }
    }
    // Десять тысяч порталов на одну машину — это не сценарий, а авария;
    // хэш от порядкового номера всё равно даёт уникальное имя.
    format!("{base}-{}", fnv1a_hex(&format!("{base}{}", taken.len())))
}

/// Присваивает порталам окончательные идентификаторы и выбрасывает записи без
/// вебхука.
///
/// Пустой вебхук в `[[portals]]` — это не «портал в процессе настройки», а
/// мусор: подключиться по нему нельзя, а вкладка с ним висела бы вечно
/// сломанной.
fn resolve_portal_ids(portals: Vec<PortalConfig>) -> Vec<PortalConfig> {
    let mut taken: Vec<String> = Vec::with_capacity(portals.len());
    let mut out: Vec<PortalConfig> = Vec::with_capacity(portals.len());
    for mut p in portals {
        if p.webhook.is_empty() {
            tracing::warn!(
                target: "forge_bitrix",
                portal = %p.id,
                "forge-bitrix: a [[portals]] entry has no webhook; skipped"
            );
            continue;
        }
        let base = if p.id.trim().is_empty() {
            portal_id_from_webhook(&p.webhook)
        } else {
            normalize_portal_id(p.id.trim())
        };
        let id = unique_portal_id(&base, &taken);
        taken.push(id.clone());
        p.id = id;
        p.title = p.title.trim().to_string();
        out.push(p);
    }
    out
}

/// Конфигурация плагина ОДНОГО портала.
///
/// Структура остаётся пер-портальной и после введения `[[portals]]`: общие
/// настройки файла (тайминги, лимиты, `cache_dir`, пути) раздаются КОПИЕЙ в
/// каждый экземпляр (см. [`BitrixRootConfig::portal_configs`]). Благодаря
/// этому сигнатура `B24State::new(cfg)` и весь код под ней не знают о
/// мультипортальности вовсе.
///
/// Все поля имеют `#[serde(default)]`: пользователь правит только то, что ему
/// нужно, а отсутствующий файл эквивалентен полным дефолтам.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BitrixConfig {
    /// URL входящего вебхука. Пусто ⇒ состояние `not_configured`.
    ///
    /// В файле живёт внутри `[[portals]]`; верхнеуровневый ключ `webhook`
    /// остаётся читаемым ради legacy-конфигов (см. [`BitrixRootConfig`]).
    pub webhook: WebhookUrl,

    /// Идентификатор портала, которому принадлежит эта конфигурация.
    ///
    /// `#[serde(skip)]` — из ВЕРХНЕГО уровня TOML не читается: он приходит из
    /// записи `[[portals]]` или выводится из вебхука. Секрета не содержит.
    #[serde(skip)]
    pub portal_id: String,

    /// Отображаемое имя портала (пусто ⇒ фронт показывает хост). Из верхнего
    /// уровня TOML не читается по той же причине, что [`Self::portal_id`].
    #[serde(skip)]
    pub portal_title: String,

    /// Переопределение базового URL портала — ЕДИНСТВЕННЫЙ способ
    /// протестировать транспорт без живого портала (стенд `tests/b24_stub.rs`,
    /// Ф2).
    ///
    /// Пусто ⇒ база берётся из самого вебхука ([`WebhookUrl::api_base`]).
    /// Правило Р17 (проверяется в Ф2): в проде обязательна схема `https`,
    /// `http://127.0.0.1` разрешён только при непустом
    /// `FORGE_BITRIX_CONFIG_PATH` (см. [`is_isolated_env`]).
    pub base_url_override: String,

    /// Каталог файлового кэша ЭТОГО портала: байты вложений, staging загрузок,
    /// курсор событий, расшифровки голосовых.
    ///
    /// С Ф6 указывает в ПОДКАТАЛОГ портала внутри
    /// [`Self::cache_root`] — см. [`Self::for_portal`]. Из TOML читается
    /// по-прежнему ОБЩИЙ корень: пользователь настраивает «где кэш», а не «где
    /// кэш каждого портала».
    pub cache_dir: PathBuf,

    /// Общий корень кэша, из которого выведен [`Self::cache_dir`] (Ф6).
    ///
    /// `#[serde(skip)]` и пустой по умолчанию. Пусто означает «этот конфиг ещё
    /// не разведён по порталам, и `cache_dir` сам себе корень» — ровно так
    /// выглядят общие настройки сразу после чтения файла и любой конфиг из
    /// тестовой фикстуры.
    ///
    /// # Зачем хранить корень, а не вычислять его отсечением компонента
    ///
    /// [`Self::for_portal`] обязана быть ИДЕМПОТЕНТНОЙ: её зовут и при сборке
    /// реестра из файла, и при добавлении портала в рантайме
    /// ([`crate::B24Registry::add`]), причём во втором случае «общие настройки»
    /// могут прийти из конфига, который уже разведён (реестр, собранный
    /// `from_states` поверх готового состояния). Без явного корня повторное
    /// применение давало бы `<кэш>/<портал>/<портал>`, и портал после рестарта
    /// не нашёл бы ни своих медиа, ни своего курсора. Отсекать же «последний
    /// компонент» нельзя: имя каталога кэша пользователь задаёт сам, и оно
    /// вполне может совпасть с идентификатором портала.
    #[serde(skip)]
    pub cache_root: PathBuf,

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

    // -----------------------------------------------------------------------
    // «Анализ чата через Claude».
    // -----------------------------------------------------------------------
    /// Файл пер-чатовых заметок, сохранённых разборов и тредов Q&A.
    ///
    /// `#[serde(skip)]` — из `bitrix.toml` НЕ читается, как и
    /// [`Self::config_path`], и по той же причине: это не настройка, а
    /// расположение пользовательских данных, и переопределяется оно только
    /// `FORGE_BITRIX_PROMPTS_PATH` (герметичность тестов, см.
    /// [`prompts_path`]). Хранится ОТДЕЛЬНЫМ полем, а не вызовом
    /// [`prompts_path`] в момент записи: иначе изменение переменной окружения
    /// на середине жизни процесса раскидало бы заметки по двум файлам.
    /// С Ф6 — файл ЭТОГО портала (`bitrix-chat-prompts-<portal>.json`),
    /// выведенный из [`Self::prompts_root`] в [`Self::for_portal`].
    #[serde(skip, default = "prompts_path")]
    pub prompts_path: PathBuf,

    /// Базовый путь заметок, из которого выведен [`Self::prompts_path`] (Ф6).
    ///
    /// Пусто ⇒ `prompts_path` сам себе база. Существует по той же причине, что
    /// и [`Self::cache_root`]: без него повторное применение
    /// [`Self::for_portal`] дало бы `bitrix-chat-prompts-acme-acme.json`.
    ///
    /// Именно СЮДА попадает `FORGE_BITRIX_PROMPTS_PATH`: переопределение
    /// берётся за БАЗУ, а имя портала подставляется уже в него — поэтому
    /// герметичность тестов сохраняется и при N порталах.
    #[serde(skip)]
    pub prompts_root: PathBuf,

    /// Файл пользовательских настроек хоста (`~/.forge/user_settings.json`).
    ///
    /// Плагин его только ЧИТАЕТ: оттуда берутся тумблер «Автоматически
    /// разбирать новые аудиосообщения» и белый список видимости диалогов
    /// (`forge_voice::prefs`, [`crate::voice_auto`]). Своего канала настроек
    /// у плагина нет — зависимость на `AppState` ради одного булева значения
    /// сломала бы границу плагина.
    ///
    /// Из TOML НЕ читается и хранится отдельным полем по той же причине, что
    /// [`Self::prompts_path`]: тест указывает свой tempdir и не может задеть
    /// настоящие настройки пользователя.
    #[serde(skip, default = "user_settings_path")]
    pub user_settings_path: PathBuf,

    /// Путь бинаря Claude CLI.
    ///
    /// В ОТЛИЧИЕ от [`Self::prompts_path`] читается из TOML: это настройка
    /// машины (у кого-то CLI лежит в `/opt`, у кого-то в `~/.bun/bin`), а не
    /// расположение пользовательских данных.
    #[serde(default = "default_claude_cli_path")]
    pub claude_cli_path: PathBuf,

    /// Таймаут одного анализа, секунды. `0` ⇒ без таймаута.
    #[serde(default = "default_analysis_timeout_secs")]
    pub analysis_timeout_secs: u64,

    /// Потолок транскрипта в CODE POINTS.
    #[serde(default = "default_analysis_max_chars")]
    pub analysis_max_chars: usize,

    /// Модель Claude (`--model`). `None` ⇒ флаг не передаётся вовсе, работает
    /// дефолт самого CLI.
    #[serde(default = "default_analysis_model")]
    pub analysis_model: Option<String>,

    /// Разрешать ли LLM-запуски для анализа чата (`claude -p`/`kimi -p`/
    /// облако). Default `false`: запуски дорогие и по умолчанию выключены
    /// глобально (см. шапку `forge_claude_runner`). Включается здесь, либо для
    /// ВСЕХ плагинов разом через `FORGE_CLAUDE_RUNNER_ENABLED=1` (приоритет).
    /// Это МАСТЕР-выключатель: какой бы `llm_backend` ни был выбран, без этого
    /// флага анализы не стартуют.
    #[serde(default)]
    pub claude_enabled: bool,

    /// Какой LLM-бэкенд использовать для анализа: `cloud` (**дефолт** —
    /// OpenAI-совместимый polza.ai, ключ резолвится из `llm_cloud_api_key` →
    /// env `FORGE_LLM_CLOUD_API_KEY`/`FORGE_MEET_POLZA_KEY` → `polza_api_key`
    /// в `~/.config/forge/meet.toml`), `claude` (локальный Claude CLI) или
    /// `kimi` (локальный Kimi CLI).
    #[serde(default = "default_llm_backend")]
    pub llm_backend: forge_claude_runner::LlmBackend,

    /// Путь бинаря Kimi CLI — используется когда `llm_backend = "kimi"`.
    #[serde(default = "default_kimi_cli_path")]
    pub kimi_cli_path: PathBuf,

    /// База облачного API для `llm_backend = "cloud"`. Пустая строка — дефолт
    /// polza.ai (`forge_voice::cloud::DEFAULT_BASE_URL`).
    #[serde(default)]
    pub llm_cloud_base_url: String,

    /// Ключ облачного API для `llm_backend = "cloud"`. Пустой — облако не
    /// настроено, анализ откажет с понятной ошибкой. В логи не пишется.
    #[serde(default)]
    pub llm_cloud_api_key: String,

    /// Модель облачного API для `llm_backend = "cloud"`. Пустая — дефолт
    /// `forge_voice::cloud::DEFAULT_SUMMARY_MODEL`.
    #[serde(default)]
    pub llm_cloud_model: String,
}

/// Дефолтный путь Kimi CLI — зеркало [`forge_claude_runner::CliKind::Kimi`].
fn default_kimi_cli_path() -> PathBuf {
    forge_claude_runner::CliKind::Kimi.default_path()
}

/// Дефолтный LLM-бэкенд: облако polza.ai. Отдельная fn (а не
/// `LlmBackend::default()`, который остаётся `Claude` ради семантики meet).
fn default_llm_backend() -> forge_claude_runner::LlmBackend {
    forge_claude_runner::LlmBackend::Cloud
}

impl Default for BitrixConfig {
    fn default() -> Self {
        Self {
            webhook: WebhookUrl::default(),
            portal_id: String::new(),
            portal_title: String::new(),
            base_url_override: String::new(),
            cache_dir: default_cache_dir(),
            cache_root: PathBuf::new(),
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
            prompts_path: prompts_path(),
            prompts_root: PathBuf::new(),
            user_settings_path: user_settings_path(),
            claude_cli_path: default_claude_cli_path(),
            analysis_timeout_secs: default_analysis_timeout_secs(),
            analysis_max_chars: default_analysis_max_chars(),
            analysis_model: default_analysis_model(),
            claude_enabled: false,
            llm_backend: default_llm_backend(),
            kimi_cli_path: default_kimi_cli_path(),
            llm_cloud_base_url: String::new(),
            llm_cloud_api_key: String::new(),
            llm_cloud_model: String::new(),
        }
    }
}

impl BitrixConfig {
    /// Общие настройки + личность ОДНОГО портала: единственная точка, где
    /// дисковые пути разводятся по порталам (Ф6).
    ///
    /// Возвращает КОПИЮ: `BitrixConfig` остаётся пер-портальной структурой, и
    /// именно на этом держится неизменность всего кода под `B24State` — ни
    /// [`crate::media_cache`], ни [`crate::routes::upload`], ни
    /// [`crate::events`] не знают, что порталов стало много. Они по-прежнему
    /// строят путь от `cache_dir`; разница лишь в том, что `cache_dir` теперь у
    /// каждого свой.
    ///
    /// Разводятся ДВА пути:
    ///
    /// - [`Self::cache_dir`] ⇒ `<общий кэш>/<портал>/` — медиа, staging
    ///   загрузок, курсор событий, расшифровки голосовых;
    /// - [`Self::prompts_path`] ⇒ `<база>-<портал>.json` — заметки и разборы
    ///   анализа.
    ///
    /// Корни (`cache_root`, `prompts_root`) запоминаются, поэтому функция
    /// ИДЕМПОТЕНТНА: применённая к уже разведённому конфигу, она перестраивает
    /// пути от того же корня, а не вкладывает подкаталог в подкаталог.
    ///
    /// # Что НЕ разводится
    ///
    /// `config_path` (файл секретов у всех порталов один — массив `[[portals]]`
    /// в нём же), `user_settings_path` (настройки хоста общие) и `voice.toml`
    /// рядом с конфигом (потолки расшифровки — настройка машины, а не портала).
    #[must_use]
    pub fn for_portal(&self, id: &str, title: &str, webhook: &WebhookUrl) -> Self {
        let mut cfg = self.clone();
        cfg.webhook = webhook.clone();
        cfg.portal_id = id.to_string();
        cfg.portal_title = title.trim().to_string();

        let cache_root = if cfg.cache_root.as_os_str().is_empty() {
            cfg.cache_dir.clone()
        } else {
            cfg.cache_root.clone()
        };
        cfg.cache_dir = portal_cache_dir(&cache_root, id);
        cfg.cache_root = cache_root;

        let prompts_root = if cfg.prompts_root.as_os_str().is_empty() {
            cfg.prompts_path.clone()
        } else {
            cfg.prompts_root.clone()
        };
        cfg.prompts_path = portal_prompts_path(&prompts_root, id);
        cfg.prompts_root = prompts_root;

        cfg
    }

    /// Полная загрузка: defaults → optional TOML → env-override →
    /// validate_and_fix.
    ///
    /// Никогда не паникует и не возвращает ошибку: невалидные значения
    /// чинятся дефолтами с warn-логом. Отсутствие файла — штатный путь: у
    /// большинства пользователей `bitrix.toml` не существует, вебхук
    /// вставляется в UI.
    pub fn load() -> Self {
        BitrixRootConfig::load().primary()
    }

    /// Читает конфиг ПЕРВОГО портала из TOML-файла.
    ///
    /// Файла нет — дефолты (не ошибка). Файл битый — warn и ПОЛНЫЕ дефолты:
    /// частичного применения нет, иначе половина конфига оказалась бы из
    /// файла, а половина — из головы, и объяснить поведение стало бы нечем.
    ///
    /// Вебхук берётся из первой записи `[[portals]]`, а при её отсутствии — из
    /// верхнеуровневого legacy-ключа `webhook`. Все порталы разом читает
    /// [`BitrixRootConfig::load_from_file`].
    pub fn load_from_file(path: &Path) -> Self {
        BitrixRootConfig::load_from_file(path).primary()
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
        if let Ok(v) = std::env::var("FORGE_BITRIX_PROMPTS_PATH") {
            if !v.is_empty() {
                self.prompts_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_CLAUDE_CLI_PATH") {
            if !v.is_empty() {
                self.claude_cli_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_CLAUDE_ENABLED") {
            match v.parse::<bool>() {
                Ok(b) => self.claude_enabled = b,
                Err(_) => tracing::warn!(
                    value = %v,
                    "FORGE_BITRIX_CLAUDE_ENABLED: not a bool, keeping {}",
                    self.claude_enabled
                ),
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_LLM_BACKEND") {
            match forge_claude_runner::LlmBackend::parse(&v) {
                Some(b) => self.llm_backend = b,
                None => tracing::warn!(
                    value = %v,
                    "FORGE_BITRIX_LLM_BACKEND: expected claude|kimi|cloud, keeping {}",
                    self.llm_backend.as_str()
                ),
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_KIMI_CLI_PATH") {
            if !v.is_empty() {
                self.kimi_cli_path = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_LLM_CLOUD_BASE_URL") {
            self.llm_cloud_base_url = v;
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_LLM_CLOUD_API_KEY") {
            self.llm_cloud_api_key = v;
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_LLM_CLOUD_MODEL") {
            self.llm_cloud_model = v;
        }
        if let Ok(v) = std::env::var("FORGE_BITRIX_ANALYSIS_MODEL") {
            // Пустая строка здесь ЗНАЧИМА: `FORGE_BITRIX_ANALYSIS_MODEL=` —
            // это «не передавать --model вовсе», единственный способ вернуться
            // к дефолту самого CLI, не редактируя bitrix.toml.
            let t = v.trim();
            self.analysis_model = (!t.is_empty()).then(|| t.to_string());
        }
        apply_env_u64(
            "FORGE_BITRIX_ANALYSIS_TIMEOUT_SECS",
            &mut self.analysis_timeout_secs,
        );
        {
            let mut chars = self.analysis_max_chars as u64;
            apply_env_u64("FORGE_BITRIX_ANALYSIS_MAX_CHARS", &mut chars);
            self.analysis_max_chars = usize::try_from(chars).unwrap_or(usize::MAX);
        }
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
        // Корни чинятся ровно так же, как выведенные из них пути: в штатном
        // порядке (`validate_and_fix` → `for_portal`) они ещё пусты, но конфиг
        // валидируют и повторно, уже разведённым по порталам.
        if !self.cache_root.as_os_str().is_empty() {
            self.cache_root = expand_tilde(&self.cache_root);
        }
        if !self.prompts_root.as_os_str().is_empty() {
            self.prompts_root = expand_tilde(&self.prompts_root);
        }
        if self.config_path.as_os_str().is_empty() {
            self.config_path = config_path();
        }
        // Пустой путь означал бы запись в «» — то есть тихую потерю всех
        // заметок; относительный превратился бы в РАЗНЫЕ файлы при разном
        // рабочем каталоге процесса (devforge запускают и из репозитория, и из
        // systemd). Поэтому путь приводится к абсолютному ЗДЕСЬ, один раз.
        self.prompts_path = expand_tilde(&self.prompts_path);
        if self.prompts_path.as_os_str().is_empty() {
            tracing::warn!(
                target: "forge_bitrix",
                "forge-bitrix: empty prompts_path; falling back to the default"
            );
            self.prompts_path = default_prompts_path();
        }
        if self.prompts_path.is_relative() {
            if let Ok(cwd) = std::env::current_dir() {
                self.prompts_path = cwd.join(&self.prompts_path);
            }
        }
        self.claude_cli_path = expand_tilde(&self.claude_cli_path);
        if self.claude_cli_path.as_os_str().is_empty() {
            tracing::warn!(
                target: "forge_bitrix",
                "forge-bitrix: empty claude_cli_path; falling back to the default"
            );
            self.claude_cli_path = default_claude_cli_path();
        }
        // Существование бинаря ЗДЕСЬ не проверяется намеренно: анализ — не
        // условие работы вкладки, и отсутствие CLI не имеет права ни ронять
        // старт, ни подменять путь. Отказ выносит роут, кодом
        // `409 claude_unavailable`, и называет ровно этот путь.
        if self.analysis_max_chars < MIN_ANALYSIS_MAX_CHARS
            || self.analysis_max_chars > MAX_ANALYSIS_MAX_CHARS
        {
            tracing::warn!(
                target: "forge_bitrix",
                value = self.analysis_max_chars,
                "forge-bitrix: analysis_max_chars is out of range; clamping"
            );
            self.analysis_max_chars = self
                .analysis_max_chars
                .clamp(MIN_ANALYSIS_MAX_CHARS, MAX_ANALYSIS_MAX_CHARS);
        }
        // `0` — легальное значение («таймаута нет»), поэтому чинится только
        // пустая/пробельная модель: `--model ""` CLI отвергает целиком.
        if let Some(model) = &self.analysis_model {
            let t = model.trim();
            if t.is_empty() {
                tracing::warn!(
                    target: "forge_bitrix",
                    "forge-bitrix: empty analysis_model; falling back to the default"
                );
                self.analysis_model = default_analysis_model();
            } else if t.len() != model.len() {
                self.analysis_model = Some(t.to_string());
            }
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
// Верхний уровень файла: общие настройки + массив порталов.
// ---------------------------------------------------------------------------

/// Разобранный `bitrix.toml` целиком: общие настройки и список порталов.
///
/// # Зачем отдельный тип
///
/// В файле ДВА уровня: корневые ключи — это настройки МАШИНЫ (тайминги
/// опроса, лимиты, `cache_dir`, пути к CLI и хранилищам), а `[[portals]]` —
/// список подключений. Разводить их по типам нужно затем, чтобы общие
/// настройки не пришлось дублировать в каждой записи портала: пользователь
/// правит `poll_active_secs` один раз, а получают его все порталы.
///
/// [`Self::portal_configs`] раздаёт общие настройки КОПИЕЙ в пер-портальные
/// [`BitrixConfig`] — поэтому `B24State` продолжает получать ровно ту же
/// структуру, что и до мультипортальности.
///
/// # Legacy
///
/// Файл без `[[portals]]`, но с верхнеуровневым `webhook`, читается как ОДИН
/// портал (`migrated_from_legacy = true`). Чтение неразрушающее: файл
/// переписывается только при первой ЗАПИСИ (см. [`save_portals_to`]).
#[derive(Debug, Clone, Default)]
pub struct BitrixRootConfig {
    /// Общие настройки. Поле `webhook` здесь — legacy-ключ верхнего уровня
    /// (или значение `FORGE_BITRIX_WEBHOOK`); пер-портальный вебхук берётся
    /// из [`Self::portals`].
    pub shared: BitrixConfig,

    /// Порталы в порядке файла, с уже присвоенными уникальными `id`.
    pub portals: Vec<PortalConfig>,

    /// Список порталов синтезирован из legacy-ключа `webhook`.
    pub migrated_from_legacy: bool,
}

impl BitrixRootConfig {
    /// Полная загрузка: файл → env-override → `validate_and_fix`.
    ///
    /// Непустой `FORGE_BITRIX_WEBHOOK` ПОБЕЖДАЕТ файл целиком и даёт ровно
    /// один портал: переменная существует ради тестового стенда, и «стенд
    /// плюс живые порталы пользователя» — не режим, а способ случайно сходить
    /// в боевой портал из теста.
    pub fn load() -> Self {
        let path = config_path();
        let mut root = Self::load_from_file(&path);
        // Читаем и пишем ОДИН файл: иначе logout стирал бы вебхук не оттуда,
        // откуда он был прочитан.
        root.shared.config_path = path;
        root.shared.apply_env();
        root.shared.validate_and_fix();
        if let Ok(v) = std::env::var("FORGE_BITRIX_WEBHOOK") {
            if !v.trim().is_empty() {
                root.portals = vec![PortalConfig::from_webhook(&root.shared.webhook, None)];
                root.migrated_from_legacy = false;
            }
        }
        root
    }

    /// Читает файл: права → чтение → разбор общих настроек и `[[portals]]`.
    ///
    /// Ни env-override, ни `validate_and_fix` здесь НЕ применяются — это
    /// делает [`Self::load`]. Тесты пользуются этим, чтобы увидеть ровно то,
    /// что лежит в файле.
    pub fn load_from_file(path: &Path) -> Self {
        // Права чинятся ДО чтения: файл мог быть создан руками (`echo >`),
        // скопирован с другой машины или восстановлен из архива, и тогда
        // секрет уже лежит доступным для чтения всей группе.
        tighten_perms_if_needed(path);

        let body = match std::fs::read_to_string(path) {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    "forge-bitrix: bitrix.toml not found, using defaults"
                );
                return Self::defaults_at(path);
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    error = %e,
                    "forge-bitrix: failed to read bitrix.toml; using defaults"
                );
                return Self::defaults_at(path);
            }
        };

        // Один разбор на оба уровня: сырая таблица нужна для `[[portals]]`
        // (она же используется на записи), типизированная — для общих
        // настроек.
        let table = match toml::from_str::<toml::Table>(&body) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    error = %e,
                    "forge-bitrix: failed to parse bitrix.toml; using defaults"
                );
                return Self::defaults_at(path);
            }
        };

        let mut shared = match toml::Value::Table(table.clone()).try_into::<BitrixConfig>() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %path.display(),
                    error = %e,
                    "forge-bitrix: failed to parse bitrix.toml; using defaults"
                );
                BitrixConfig::default()
            }
        };
        shared.config_path = path.to_path_buf();

        let (portals, migrated_from_legacy) = portals_from_table(&table);
        tracing::info!(
            target: "forge_bitrix",
            path = %path.display(),
            portals = portals.len(),
            legacy = migrated_from_legacy,
            "forge-bitrix: loaded config from TOML"
        );
        Self {
            shared,
            portals,
            migrated_from_legacy,
        }
    }

    fn defaults_at(path: &Path) -> Self {
        Self {
            shared: BitrixConfig {
                config_path: path.to_path_buf(),
                ..BitrixConfig::default()
            },
            portals: Vec::new(),
            migrated_from_legacy: false,
        }
    }

    /// Идентификаторы порталов в порядке файла. Секрета не содержат.
    pub fn portal_ids(&self) -> Vec<String> {
        self.portals.iter().map(|p| p.id.clone()).collect()
    }

    /// Пер-портальные конфигурации: общие настройки КОПИЕЙ + вебхук, `id` и
    /// `title` конкретного портала.
    ///
    /// Копия, а не ссылка, намеренно: Ф6 разводит `cache_dir` по подкаталогам
    /// порталов, и для этого пер-портальной структуре нужно СВОЁ значение
    /// поля, а не общее на всех. Разведением занимается
    /// [`BitrixConfig::for_portal`].
    pub fn portal_configs(&self) -> Vec<BitrixConfig> {
        self.portals
            .iter()
            .map(|p| self.shared.for_portal(&p.id, &p.title, &p.webhook))
            .collect()
    }

    /// Конфигурация первого портала — совместимость с однопортальным кодом.
    ///
    /// Порталов нет ⇒ общие настройки с ПУСТЫМ вебхуком: состояние
    /// `not_configured`, ровно как до мультипортальности.
    pub fn primary(&self) -> BitrixConfig {
        match self.portal_configs().into_iter().next() {
            Some(cfg) => cfg,
            // Пустой слот тоже получает СВОЙ каталог (зарезервированный
            // `_vacant`): писать в общий корень, вперемешку с подкаталогами
            // порталов, не должен никто.
            None => self
                .shared
                .for_portal("", "", &WebhookUrl::default()),
        }
    }
}

/// Достаёт порталы из сырой таблицы, мигрируя legacy-ключ В ПАМЯТИ.
///
/// Возвращает `(порталы, миграция_из_legacy)`. Файл при этом не трогается:
/// чтение обязано оставаться неразрушающим — конфиг читают и однократные
/// диагностические запуски, которым нечего переписывать чужой файл.
fn portals_from_table(table: &toml::Table) -> (Vec<PortalConfig>, bool) {
    let listed: Vec<PortalConfig> = table
        .get(PORTALS_KEY)
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v.clone().try_into::<PortalConfig>() {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(
                            target: "forge_bitrix",
                            error = %e,
                            "forge-bitrix: a [[portals]] entry is malformed; skipped"
                        );
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let resolved = resolve_portal_ids(listed);
    if !resolved.is_empty() {
        return (resolved, false);
    }

    // Legacy: верхнеуровневый `webhook` — это ровно один портал.
    let legacy = table
        .get(WEBHOOK_KEY)
        .and_then(toml::Value::as_str)
        .map(WebhookUrl::new)
        .filter(|w| !w.is_empty());
    match legacy {
        Some(w) => (resolve_portal_ids(vec![PortalConfig::from_webhook(&w, None)]), true),
        None => (Vec::new(), false),
    }
}

// ---------------------------------------------------------------------------
// Запись секрета на диск: единственный путь, которым вебхук покидает процесс.
// ---------------------------------------------------------------------------

/// Ключ вебхука в TOML-файле. Совпадает с именем поля [`BitrixConfig`]:
/// read-modify-write работает с сырой таблицей, и разъехавшееся имя тихо
/// создало бы ВТОРОЙ ключ вместо перезаписи первого.
pub const WEBHOOK_KEY: &str = "webhook";

/// Ключ массива порталов в TOML-файле (`[[portals]]`).
pub const PORTALS_KEY: &str = "portals";

/// Ключ идентификатора портала внутри записи `[[portals]]`.
pub const PORTAL_ID_KEY: &str = "id";

/// Ключ отображаемого имени портала внутри записи `[[portals]]`.
pub const PORTAL_TITLE_KEY: &str = "title";

/// Атомарно записывает ВЕСЬ список порталов в TOML-файл `path` с правами
/// `0600`.
///
/// Единственная функция, которая кладёт секреты в файл; [`save_webhook_to`],
/// [`add_portal_to`] и [`remove_portal_from`] — обёртки над ней.
///
/// Инварианты (ради них функция и существует отдельно от `serde`) перечислены
/// в шапке модуля. Ключевые следствия:
///
/// - каталог (`~/.config/forge`) создаётся при необходимости;
/// - остальные ключи файла сохраняются как есть — пользователь мог задать
///   `poll_active_secs`, `cache_dir` и что угодно ещё;
/// - массив `[[portals]]` перезаписывается ЦЕЛИКОМ значением аргумента:
///   вызывающий всегда работает от свежепрочитанного списка;
/// - верхнеуровневый legacy-ключ `webhook` СНИМАЕТСЯ — иначе тот же секрет
///   остался бы в файле в двух экземплярах, и стереть его пришлось бы дважды;
/// - в лог уходят только `id` и [`WebhookUrl::fingerprint`], никогда сам URL.
///
/// # Errors
///
/// Пустой вебхук у любой записи (стирать нужно через [`clear_webhook_to`] или
/// [`remove_portal_from`]); невозможность создать каталог или файл; ошибка
/// записи, `fsync` или `rename`.
pub fn save_portals_to(path: &Path, portals: &[PortalConfig]) -> anyhow::Result<()> {
    // Проверка ДО чтения файла: отказ не должен оставлять после себя ни
    // созданного каталога, ни пустого конфига.
    for p in portals {
        if p.webhook.is_empty() {
            anyhow::bail!("the webhook must not be empty");
        }
    }

    let mut doc = read_table(path)?;
    // Миграция legacy: первая же запись убирает верхнеуровневый ключ.
    let had_legacy = doc.remove(WEBHOOK_KEY).is_some();

    if portals.is_empty() {
        doc.remove(PORTALS_KEY);
    } else {
        let arr: Vec<toml::Value> = portals
            .iter()
            .map(|p| {
                let mut t = toml::Table::new();
                t.insert(
                    PORTAL_ID_KEY.into(),
                    toml::Value::String(p.id.clone()),
                );
                if !p.title.trim().is_empty() {
                    t.insert(
                        PORTAL_TITLE_KEY.into(),
                        toml::Value::String(p.title.trim().to_string()),
                    );
                }
                t.insert(
                    WEBHOOK_KEY.into(),
                    // ЕДИНСТВЕННОЕ место во всём крейте, где секрет
                    // превращается в сериализуемое значение.
                    toml::Value::String(p.webhook.expose().to_string()),
                );
                toml::Value::Table(t)
            })
            .collect();
        doc.insert(PORTALS_KEY.into(), toml::Value::Array(arr));
    }

    write_table(path, &doc)?;
    tracing::info!(
        target: "forge_bitrix",
        path = %path.display(),
        portals = portals.len(),
        migrated = had_legacy,
        ids = %portals.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(","),
        fingerprints = %portals.iter().map(|p| p.webhook.fingerprint()).collect::<Vec<_>>().join(","),
        "forge-bitrix: saved the portals (only fingerprints are ever logged)"
    );
    Ok(())
}

/// Читает список порталов из файла ДЛЯ ЗАПИСИ, мигрируя legacy-ключ.
///
/// Отличается от [`BitrixRootConfig::load_from_file`] только тем, что общие
/// настройки не разбираются вовсе: обновление секрета не должно падать
/// из-за опечатки в `poll_active_secs`.
fn portals_for_write(path: &Path) -> anyhow::Result<Vec<PortalConfig>> {
    let doc = read_table(path)?;
    Ok(portals_from_table(&doc).0)
}

/// Сохраняет ЕДИНСТВЕННЫЙ портал, заменяя весь список (совместимость).
///
/// Семантика та же, что была до мультипортальности: повторный вход по другому
/// вебхуку ЗАМЕНЯЕТ подключение, а не добавляет второе. Добавление —
/// отдельная и явная операция [`add_portal_to`], иначе кнопка «подключить» в
/// старом экране входа тихо плодила бы порталы.
///
/// # Errors
///
/// См. [`save_portals_to`].
pub fn save_webhook_to(path: &Path, webhook: &WebhookUrl) -> anyhow::Result<()> {
    if webhook.is_empty() {
        anyhow::bail!("the webhook must not be empty");
    }
    save_portals_to(path, &[PortalConfig::from_webhook(webhook, None)])
}

/// Добавляет портал к уже записанным, не трогая их секреты.
///
/// Идемпотентна по вебхуку: тот же URL не создаёт второй записи, а лишь
/// обновляет `title`. Идентификатор нового портала выводится из хоста и при
/// коллизии получает суффикс — уже существующие идентификаторы НЕ меняются.
///
/// Возвращает записанный портал (в частности, его окончательный `id`).
///
/// # Errors
///
/// Пустой вебхук; ошибки чтения/записи конфига.
pub fn add_portal_to(
    path: &Path,
    webhook: &WebhookUrl,
    title: Option<&str>,
) -> anyhow::Result<PortalConfig> {
    if webhook.is_empty() {
        anyhow::bail!("the webhook must not be empty");
    }
    let mut portals = portals_for_write(path)?;

    let existing = portals.iter().position(|p| p.webhook == *webhook);
    let idx = match existing {
        Some(i) => {
            if let Some(t) = title {
                portals[i].title = t.trim().to_string();
            }
            i
        }
        None => {
            let taken: Vec<String> = portals.iter().map(|p| p.id.clone()).collect();
            let id = unique_portal_id(&portal_id_from_webhook(webhook), &taken);
            portals.push(PortalConfig {
                id,
                title: title.unwrap_or_default().trim().to_string(),
                webhook: webhook.clone(),
            });
            portals.len() - 1
        }
    };

    let saved = portals[idx].clone();
    save_portals_to(path, &portals)?;
    Ok(saved)
}

/// Убирает один портал по идентификатору. Возвращает `true`, если он там был.
///
/// Отсутствующий портал и отсутствующий файл — НЕ ошибка: удаление обязано
/// быть идемпотентным, иначе повторный клик по «отключить» превращался бы в
/// красный тост.
///
/// **На портал при этом не ходим** — по той же причине, что и в
/// [`clear_webhook_to`].
///
/// # Errors
///
/// Ошибки чтения/записи конфига.
pub fn remove_portal_from(path: &Path, id: &str) -> anyhow::Result<bool> {
    let portals = portals_for_write(path)?;
    let rest: Vec<PortalConfig> = portals.iter().filter(|p| p.id != id).cloned().collect();
    if rest.len() == portals.len() {
        // Ничего не удалили. Файла может не быть вовсе — создавать его незачем.
        if !path.exists() {
            return Ok(false);
        }
        // Файл есть: он мог содержать legacy-ключ, и переписать его всё же
        // стоит только если состав порталов изменился. Не изменился — выходим.
        return Ok(false);
    }
    save_portals_to(path, &rest)?;
    Ok(true)
}

/// Переименовывает портал: меняет ТОЛЬКО `title`, не трогая ни `id`, ни секрет.
///
/// Возвращает `true`, если портал с таким `id` в файле нашёлся. Отсутствующий
/// портал — не ошибка и не повод переписывать файл.
///
/// Пустое (или из одних пробелов) имя означает «вернуть автоматическую
/// подпись»: ключ `title` из записи исчезает, и фронт снова показывает хост
/// портала ([`PortalConfig::display_name`]).
///
/// # Errors
///
/// Ошибки чтения/записи конфига (см. [`save_portals_to`]).
pub fn rename_portal_in(path: &Path, id: &str, title: &str) -> anyhow::Result<bool> {
    let mut portals = portals_for_write(path)?;
    let Some(slot) = portals.iter_mut().find(|p| p.id == id) else {
        return Ok(false);
    };
    let next = title.trim().to_string();
    if slot.title == next {
        // Идемпотентность без лишней записи: переписывать файл, чтобы получить
        // ровно те же байты, значит зря рисковать секретом.
        return Ok(true);
    }
    slot.title = next;
    save_portals_to(path, &portals)?;
    Ok(true)
}

/// Меняет СЕКРЕТ уже записанного портала, не трогая ни `id`, ни `title`, ни
/// его место в списке.
///
/// Возвращает `true`, если портал с таким `id` в файле нашёлся.
///
/// # Зачем отдельный примитив
///
/// Ровно один сценарий: вебхук отозвали на портале и выпустили заново. Хост
/// тот же, код другой — то есть это ТА ЖЕ вкладка, а не новая. Пара
/// «[`remove_portal_from`] + [`add_portal_to`]» дала бы тот же файл, но ценой
/// двух записей (между ними портал на диске не существует) и потерянных `title`
/// с позицией: `add_portal_to` дописывает в хвост.
///
/// Секрет соседей при этом не читается и не переписывается — как и во всех
/// остальных примитивах записи, файл собирается целиком из уже разобранных
/// записей ([`save_portals_to`], и все четыре его инварианта наследуются).
///
/// # Errors
///
/// Пустой вебхук; ошибки чтения/записи конфига.
pub fn replace_portal_webhook_in(
    path: &Path,
    id: &str,
    webhook: &WebhookUrl,
) -> anyhow::Result<bool> {
    if webhook.is_empty() {
        anyhow::bail!("the webhook must not be empty");
    }
    let mut portals = portals_for_write(path)?;
    let Some(slot) = portals.iter_mut().find(|p| p.id == id) else {
        return Ok(false);
    };
    if slot.webhook == *webhook {
        // Идемпотентность без лишней записи: переписывать файл, чтобы получить
        // ровно те же байты, значит зря рисковать секретом.
        return Ok(true);
    }
    slot.webhook = webhook.clone();
    save_portals_to(path, &portals)?;
    Ok(true)
}

/// Переставляет порталы в файле в порядке `ids` (порядок вкладок во фронте).
///
/// Порядок в файле — ЕДИНСТВЕННОЕ место, где он живёт, поэтому перестановка
/// вкладок обязана доезжать до диска: иначе она забывалась бы при каждом
/// перезапуске devforge.
///
/// Устойчива к неполному и грязному списку: неизвестные `id` игнорируются, а
/// не перечисленные порталы уезжают в хвост, сохраняя относительный порядок.
/// Ни один портал не теряется — список приходит из фронта, а фронт мог
/// отстать от состава реестра на один запрос.
///
/// # Errors
///
/// Ошибки чтения/записи конфига (см. [`save_portals_to`]).
pub fn reorder_portals_in(path: &Path, ids: &[String]) -> anyhow::Result<()> {
    let portals = portals_for_write(path)?;
    if portals.is_empty() {
        return Ok(());
    }
    let ordered = reorder_by_ids(portals, ids, |p| p.id.clone());
    save_portals_to(path, &ordered)
}

/// Переставляет `portals` в порядке `ids`, дописывая непомянутых в хвост.
///
/// Вынесена отдельно, потому что тем же правилом пользуется и реестр в памяти
/// ([`crate::B24Registry::reorder`]): разойдись эти два порядка — вкладки после
/// перезапуска встали бы иначе, чем их только что перетащили.
pub(crate) fn reorder_by_ids<T>(
    mut items: Vec<T>,
    ids: &[String],
    key: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut out = Vec::with_capacity(items.len());
    for want in ids {
        if let Some(i) = items.iter().position(|it| &key(it) == want) {
            out.push(items.remove(i));
        }
    }
    out.append(&mut items);
    out
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
    // Снимаются ОБА представления: и legacy-ключ, и весь массив порталов.
    // Logout старого экрана входа отвязывает подключение целиком; убрать один
    // портал из нескольких умеет [`remove_portal_from`].
    let had_legacy = doc.remove(WEBHOOK_KEY).is_some();
    let had_portals = doc.remove(PORTALS_KEY).is_some();
    if !had_legacy && !had_portals && !path.exists() {
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

    // -----------------------------------------------------------------------
    // «Анализ чата через Claude».
    // -----------------------------------------------------------------------

    /// Дефолты фичи применяются без файла и без переменных окружения.
    ///
    /// `prompts_path` при этом сверяется НЕ с `default_prompts_path()`, а с
    /// [`prompts_path`]: прогон `cargo test` может идти с уже выставленным
    /// `FORGE_BITRIX_PROMPTS_PATH` (герметичный стенд), и жёсткое сравнение с
    /// `~/.forge/...` красило бы тест в зависимости от окружения.
    ///
    /// С Ф6 сверка идёт с [`portal_prompts_path`] от той же базы: файл заметок
    /// стал ПЕР-ПОРТАЛЬНЫМ, а `load_from_file` отдаёт конфиг первого портала —
    /// здесь его нет вовсе, поэтому суффикс достаётся пустому слоту.
    #[test]
    fn analysis_defaults_apply_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig::load_from_file(&dir.path().join("nope.toml"));
        assert_eq!(cfg.prompts_path, portal_prompts_path(&prompts_path(), ""));
        assert_eq!(
            cfg.prompts_root,
            prompts_path(),
            "переопределение FORGE_BITRIX_PROMPTS_PATH остаётся БАЗОЙ"
        );
        assert_eq!(cfg.claude_cli_path, default_claude_cli_path());
        assert_eq!(cfg.analysis_timeout_secs, DEFAULT_ANALYSIS_TIMEOUT_SECS);
        assert_eq!(cfg.analysis_max_chars, DEFAULT_ANALYSIS_MAX_CHARS);
        assert_eq!(
            cfg.analysis_model.as_deref(),
            Some(DEFAULT_ANALYSIS_MODEL),
            "по умолчанию анализ идёт на sonnet"
        );
        // Хранилище заметок — НЕ настройка: из TOML путь не читается вовсе.
        assert!(cfg.prompts_path.is_absolute() || cfg.prompts_path.as_os_str().is_empty());
    }

    /// Настройки машины (путь CLI, таймаут, потолок, модель) читаются из TOML,
    /// а путь хранилища — НЕТ.
    #[test]
    fn analysis_settings_come_from_the_file_but_the_store_path_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            concat!(
                "claude_cli_path = \"/opt/claude\"\n",
                "analysis_timeout_secs = 42\n",
                "analysis_max_chars = 12345\n",
                "analysis_model = \"opus\"\n",
                "prompts_path = \"/tmp/hijacked.json\"\n",
                // Корни Ф6 — тоже #[serde(skip)], и по той же причине: из них
                // строятся ПУТИ НА ДИСКЕ, а конфиг мог быть скопирован с чужой
                // машины или восстановлен из архива.
                "cache_root = \"/tmp/hijacked-cache\"\n",
                "prompts_root = \"/tmp/hijacked-root.json\"\n",
            ),
        )
        .unwrap();
        let cfg = BitrixConfig::load_from_file(&path);
        assert_ne!(
            cfg.cache_root,
            PathBuf::from("/tmp/hijacked-cache"),
            "cache_root помечен #[serde(skip)] и из TOML читаться не смеет"
        );
        assert_ne!(
            cfg.prompts_root,
            PathBuf::from("/tmp/hijacked-root.json"),
            "prompts_root помечен #[serde(skip)] и из TOML читаться не смеет"
        );
        assert!(
            !cfg.cache_dir.starts_with("/tmp/hijacked-cache"),
            "и подставиться в cache_dir через разведение по порталам — тоже"
        );
        assert_eq!(cfg.claude_cli_path, PathBuf::from("/opt/claude"));
        assert_eq!(cfg.analysis_timeout_secs, 42);
        assert_eq!(cfg.analysis_max_chars, 12_345);
        assert_eq!(cfg.analysis_model.as_deref(), Some("opus"));
        assert_ne!(
            cfg.prompts_path,
            PathBuf::from("/tmp/hijacked.json"),
            "prompts_path помечен #[serde(skip)] и из TOML читаться не смеет"
        );
    }

    /// Env-override: `FORGE_BITRIX_*` побеждает и файл, и дефолты.
    ///
    /// Ветка сериализована мьютексом и восстанавливает прежние значения —
    /// переменные процесса общие на все тесты крейта.
    #[test]
    fn analysis_env_overrides_win() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        const KEYS: [&str; 6] = [
            "FORGE_BITRIX_PROMPTS_PATH",
            "FORGE_BITRIX_CLAUDE_CLI_PATH",
            "FORGE_BITRIX_CLAUDE_ENABLED",
            "FORGE_BITRIX_ANALYSIS_MODEL",
            "FORGE_BITRIX_ANALYSIS_TIMEOUT_SECS",
            "FORGE_BITRIX_ANALYSIS_MAX_CHARS",
        ];
        let previous: Vec<_> = KEYS.iter().map(|k| (*k, std::env::var_os(k))).collect();

        std::env::set_var("FORGE_BITRIX_PROMPTS_PATH", "/tmp/forge-b24-prompts.json");
        std::env::set_var("FORGE_BITRIX_CLAUDE_CLI_PATH", "/usr/local/bin/claude");
        std::env::set_var("FORGE_BITRIX_ANALYSIS_MODEL", "haiku");
        std::env::set_var("FORGE_BITRIX_ANALYSIS_TIMEOUT_SECS", "17");
        std::env::set_var("FORGE_BITRIX_ANALYSIS_MAX_CHARS", "9000");

        let mut cfg = BitrixConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.prompts_path,
            PathBuf::from("/tmp/forge-b24-prompts.json")
        );
        assert_eq!(cfg.claude_cli_path, PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(cfg.analysis_model.as_deref(), Some("haiku"));
        assert_eq!(cfg.analysis_timeout_secs, 17);
        assert_eq!(cfg.analysis_max_chars, 9_000);

        // Пустая строка ЗНАЧИМА: «не передавать --model вовсе».
        std::env::set_var("FORGE_BITRIX_ANALYSIS_MODEL", "");
        let mut cfg = BitrixConfig::default();
        cfg.apply_env();
        assert!(cfg.analysis_model.is_none());

        for (k, v) in previous {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// `FORGE_BITRIX_PROMPTS_PATH` продолжает изолировать тесты и при N
    /// порталах: переопределение берётся ЗА БАЗУ, имя портала подставляется в
    /// него.
    ///
    /// Без этого свойства Ф6 тихо пробила бы герметичность: подставь мы имя
    /// портала в `default_prompts_path()` вместо переопределения — и первый же
    /// `cargo test`, дёрнувший сохранение заметки, начал бы писать в живой
    /// `~/.forge/bitrix-chat-prompts-<портал>.json` разработчика.
    ///
    /// Ветка сериализована тем же мьютексом и восстанавливает прежние
    /// значения — переменные процесса общие на все тесты крейта.
    #[test]
    fn the_prompts_path_override_stays_the_base_for_every_portal() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        const KEY: &str = "FORGE_BITRIX_PROMPTS_PATH";
        let previous = std::env::var_os(KEY);
        let sandbox = tempfile::tempdir().unwrap();
        let base = sandbox.path().join("hermetic-prompts.json");
        std::env::set_var(KEY, &base);

        let mut shared = BitrixConfig::default();
        shared.apply_env();
        assert_eq!(shared.prompts_path, base, "переопределение прочитано");

        let alpha = shared.for_portal("alpha", "", &WebhookUrl::default());
        let beta = shared.for_portal("beta", "", &WebhookUrl::default());

        assert_eq!(alpha.prompts_path, base.with_file_name("hermetic-prompts-alpha.json"));
        assert_eq!(beta.prompts_path, base.with_file_name("hermetic-prompts-beta.json"));
        assert_ne!(alpha.prompts_path, beta.prompts_path);
        for cfg in [&alpha, &beta] {
            assert_eq!(cfg.prompts_root, base, "база сохранена целиком");
            assert!(
                cfg.prompts_path.starts_with(sandbox.path()),
                "заметки обязаны остаться в песочнице: {}",
                cfg.prompts_path.display()
            );
        }

        match previous {
            Some(v) => std::env::set_var(KEY, v),
            None => std::env::remove_var(KEY),
        }
    }

    /// Потолок транскрипта клампится, пустая модель чинится, путь хранилища
    /// становится абсолютным — и всё это без ошибки.
    #[test]
    fn analysis_values_are_repaired_not_rejected() {
        let mut c = BitrixConfig {
            analysis_max_chars: 10,
            analysis_model: Some("   ".into()),
            prompts_path: PathBuf::from("relative-prompts.json"),
            claude_cli_path: PathBuf::new(),
            ..BitrixConfig::default()
        };
        c.validate_and_fix();
        assert_eq!(c.analysis_max_chars, MIN_ANALYSIS_MAX_CHARS);
        assert_eq!(c.analysis_model.as_deref(), Some(DEFAULT_ANALYSIS_MODEL));
        assert!(c.prompts_path.is_absolute(), "{:?}", c.prompts_path);
        assert_eq!(c.claude_cli_path, default_claude_cli_path());

        let mut c = BitrixConfig {
            analysis_max_chars: MAX_ANALYSIS_MAX_CHARS + 1,
            // `0` — ЛЕГАЛЬНОЕ значение таймаута («его нет»), и чиниться не
            // должно.
            analysis_timeout_secs: 0,
            ..BitrixConfig::default()
        };
        c.validate_and_fix();
        assert_eq!(c.analysis_max_chars, MAX_ANALYSIS_MAX_CHARS);
        assert_eq!(c.analysis_timeout_secs, 0);
    }

    /// Файл хранилища создаётся с правами owner-only.
    ///
    /// Пишет его `forge_chat_analysis::prompts::save` через общую
    /// `atomic::write_bytes_atomic` — ту самую, что до этой фазы существовала
    /// у Bitrix третьей копией внутри приватной `write_table`. Тест
    /// зафиксирован здесь, потому что права — свойство ЭТОГО файла плагина.
    #[test]
    #[cfg(unix)]
    fn the_prompt_store_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/prompts.json");
        let store = forge_chat_analysis::prompts::PromptStore::default();
        forge_chat_analysis::prompts::save(&path, &store).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "заметки о людях не лежат читаемыми для всех");
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

    /// Повторное сохранение ПЕРЕЗАПИСЫВАЕТ: у legacy-ключа верхнего уровня
    /// значение ровно одно, второго ключа появиться не должно.
    ///
    /// Речь именно о [`BitrixConfig::save_webhook`] — записи ОДНОГО секрета в
    /// корень файла. Список порталов живёт в массиве `[[portals]]` и пишется
    /// другими функциями ([`add_portal_to`], [`save_portals_to`]); ограничение
    /// «портал ровно один» (Р5) снято мультипортальностью и на этот тест
    /// больше не распространяется.
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

    // -------------------------------------------------------------------
    // Мультипортальность: массив [[portals]] и общие настройки (Ф1.1).
    // -------------------------------------------------------------------

    /// Два портала разбираются в две пер-портальные конфигурации, и общие
    /// настройки достаются ОБЕИМ — ради этого `[[portals]]` и вынесен в
    /// отдельный уровень.
    #[test]
    fn two_portals_share_the_common_settings_and_differ_only_by_the_portal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            r#"
poll_active_secs = 9
recent_sync_secs = 111
max_upload_bytes = 12345
base_url_override = "https://stand.example/rest/1/x"

[[portals]]
title = "Первый"
webhook = "https://acme.bitrix24.ru/rest/1/aaa111aaa111/"

[[portals]]
id = "beta"
webhook = "https://beta.bitrix24.ru/rest/9/bbb222bbb222/"
"#,
        )
        .unwrap();

        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme", "beta"]);
        assert!(!root.migrated_from_legacy);

        let cfgs = root.portal_configs();
        assert_eq!(cfgs.len(), 2);
        for c in &cfgs {
            // Общие настройки СКОПИРОВАНЫ, а не потеряны на N=2.
            assert_eq!(c.poll_active_secs, 9);
            assert_eq!(c.recent_sync_secs, 111);
            assert_eq!(c.max_upload_bytes, 12345);
            assert_eq!(c.base_url_override, "https://stand.example/rest/1/x");
            assert_eq!(c.config_path, path);
        }
        // Различаются ровно порталом.
        assert_eq!(cfgs[0].portal_id, "acme");
        assert_eq!(cfgs[0].portal_title, "Первый");
        assert_eq!(
            cfgs[0].webhook.expose(),
            "https://acme.bitrix24.ru/rest/1/aaa111aaa111/"
        );
        assert_eq!(cfgs[1].portal_id, "beta");
        assert_eq!(cfgs[1].portal_title, "");
        assert_eq!(
            cfgs[1].webhook.expose(),
            "https://beta.bitrix24.ru/rest/9/bbb222bbb222/"
        );

        // Однопортальный код видит первый портал и ведёт себя как раньше.
        let primary = BitrixConfig::load_from_file(&path);
        assert_eq!(primary.portal_id, "acme");
        assert!(primary.has_webhook());
    }

    /// `WebhookUrl` не сериализуем — положить его в DTO нельзя.
    ///
    /// Проверка компилируемая: функция принимает только `T: Serialize`, и
    /// раскомментированный вызов с `WebhookUrl` перестал бы собираться. Тест
    /// сторожит ИМЕННО отсутствие impl'а: добавить `#[derive(Serialize)]`
    /// молча уже не выйдет — рядом лежит утверждение о `PortalConfig`.
    #[test]
    fn the_webhook_still_cannot_be_serialized() {
        fn assert_serializable<T: serde::Serialize>(_: &T) {}
        // Сериализуемое — сериализуется.
        assert_serializable(&String::from("ok"));
        // А это не компилируется, и так и должно быть:
        // assert_serializable(&WebhookUrl::new("https://p/rest/1/x/"));
        // assert_serializable(&PortalConfig::default());
        // assert_serializable(&BitrixConfig::default());

        // Единственный путь наружу — expose(), и он же единственный, по
        // которому секрет попадает в TOML-значение.
        let w = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123/");
        assert_eq!(w.expose(), "https://p.bitrix24.ru/rest/1/abc123/");
        assert_eq!(format!("{w:?}"), "<redacted>");
    }

    /// Запись `[[portals]]` без вебхука — мусор, а не «портал в настройке».
    #[test]
    fn a_portal_without_a_webhook_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "[[portals]]\nid = \"empty\"\nwebhook = \"\"\n\n[[portals]]\nwebhook = \"https://ok.bitrix24.ru/rest/1/ccc333ccc333/\"\n",
        )
        .unwrap();

        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["ok"]);
    }

    /// Порталов нет вовсе ⇒ `not_configured` и пустой список, а не паника.
    #[test]
    fn a_config_without_portals_yields_an_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(&path, "poll_idle_secs = 33\n").unwrap();

        let root = BitrixRootConfig::load_from_file(&path);
        assert!(root.portals.is_empty());
        let primary = root.primary();
        assert!(!primary.has_webhook());
        assert_eq!(primary.portal_id, "");
        assert_eq!(primary.poll_idle_secs, 33);
    }

    // -------------------------------------------------------------------
    // Вывод идентификатора портала (Ф1.2).
    // -------------------------------------------------------------------

    /// Идентификатор берётся из ХОСТА и ни одной подстроки пути в себе не
    /// несёт — на этом держится право показывать его во фронте и в логах.
    #[test]
    fn the_portal_id_comes_from_the_host_and_never_from_the_path() {
        let w = WebhookUrl::new("https://test-work-2.bitrix24.ru/rest/1387/qwertyuiopas/");
        let id = portal_id_from_webhook(&w);
        assert_eq!(id, "test-work-2");
        assert!(!id.contains("1387"), "{id}");
        assert!(!id.contains("qwertyuiopas"), "{id}");

        // И наоборот: ни один кусок пути не встречается в идентификаторе.
        let (_, uid, code) = w.parts().unwrap();
        for n in 3..=code.len() {
            assert!(!id.contains(&code[..n]), "код просочился в id: {id}");
        }
        assert!(!id.contains(uid), "{id}");
    }

    /// Коллизия разрешается суффиксом, а ПЕРВЫЙ идентификатор не переезжает:
    /// вместе с ним переехали бы его кэш и настройки.
    #[test]
    fn two_webhooks_of_the_same_portal_get_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let first = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/");
        let second = WebhookUrl::new("https://acme.bitrix24.ru/rest/77/bbb222bbb222/");

        let a = add_portal_to(&path, &first, None).unwrap();
        let b = add_portal_to(&path, &second, None).unwrap();
        assert_eq!(a.id, "acme");
        assert_eq!(b.id, "acme-2");

        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme", "acme-2"]);
        // Первый портал сохранил и идентификатор, и секрет.
        assert_eq!(root.portals[0].webhook.expose(), first.expose());
        assert_eq!(root.portals[1].webhook.expose(), second.expose());
    }

    /// Экзотические хосты дают валидный идентификатор: IP, порт, поддомены,
    /// IPv6 и IDN.
    #[test]
    fn exotic_hosts_still_yield_a_usable_id() {
        let cases = [
            ("test.bitrix24.ru", "test"),
            ("TEST-Work-2.bitrix24.ru", "test-work-2"),
            ("a.b.c.example.com", "a"),
            ("192.168.1.5", "192-168-1-5"),
            ("192.168.1.5:8081", "192-168-1-5"),
            ("127.0.0.1:8080", "127-0-0-1"),
            ("[::1]:8080", "1"),
            ("example.com:8443", "example"),
        ];
        for (host, want) in cases {
            let got = portal_id_from_host(host);
            assert_eq!(got, want, "host={host}");
            assert!(
                !got.is_empty()
                    && got
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "host={host} id={got}"
            );
        }

        // IDN: ни одной ASCII-буквоцифры — детерминированный фолбэк.
        let idn = portal_id_from_host("портал.рф");
        assert!(idn.starts_with("portal-"), "{idn}");
        assert_eq!(idn, portal_id_from_host("портал.рф"), "фолбэк не стабилен");
        assert_ne!(idn, portal_id_from_host("другой.рф"));

        // Нераспознанный вебхук не хэшируется вовсе — хэшировать было бы
        // нечего, кроме секрета.
        assert_eq!(
            portal_id_from_webhook(&WebhookUrl::new("не вебхук вовсе")),
            FALLBACK_PORTAL_ID
        );
    }

    /// Явно заданный в файле `id` нормализуется, но не выдумывается заново.
    #[test]
    fn an_explicit_id_is_normalized_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "[[portals]]\nid = \"My Portal!\"\nwebhook = \"https://acme.bitrix24.ru/rest/1/aaa111aaa111/\"\n",
        )
        .unwrap();
        assert_eq!(BitrixRootConfig::load_from_file(&path).portal_ids(), vec!["my-portal"]);
    }

    // -------------------------------------------------------------------
    // Запись N порталов (Ф1.3).
    // -------------------------------------------------------------------

    /// Добавление второго портала не трогает ни секрет первого, ни посторонние
    /// ключи файла.
    #[test]
    fn adding_a_second_portal_touches_neither_the_first_secret_nor_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "poll_active_secs = 7\nsomething_from_the_future = \"keep me\"\n",
        )
        .unwrap();

        let first = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/");
        let second = WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/");
        add_portal_to(&path, &first, Some("Acme")).unwrap();
        add_portal_to(&path, &second, None).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("poll_active_secs = 7"), "{body}");
        assert!(body.contains("something_from_the_future"), "{body}");
        assert!(body.contains("aaa111aaa111"), "секрет первого портала потерян");
        assert!(body.contains("bbb222bbb222"), "{body}");

        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme", "beta"]);
        assert_eq!(root.portals[0].title, "Acme");
        assert_eq!(root.shared.poll_active_secs, 7);

        // Тот же вебхук второй раз — не второй портал, а обновление имени.
        let again = add_portal_to(&path, &first, Some("Acme Inc")).unwrap();
        assert_eq!(again.id, "acme");
        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme", "beta"]);
        assert_eq!(root.portals[0].title, "Acme Inc");
    }

    /// Права `0600` держатся и на файле, созданном с нуля записью МАССИВА, и
    /// на испорченном снаружи.
    #[cfg(unix)]
    #[test]
    fn writing_several_portals_keeps_the_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/forge/bitrix.toml");
        let portals = vec![
            PortalConfig::from_webhook(
                &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/"),
                Some("Acme"),
            ),
            PortalConfig::from_webhook(
                &WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/"),
                None,
            ),
        ];

        save_portals_to(&path, &portals).unwrap();
        assert_eq!(mode_of(&path), 0o600, "секрет обязан лежать 0600");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_portals_to(&path, &portals).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    /// Отказ на этапе записи не оставляет ни временного файла, ни изменённого
    /// целевого.
    #[test]
    fn a_failed_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let good = PortalConfig::from_webhook(
            &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/"),
            None,
        );
        save_portals_to(&path, std::slice::from_ref(&good)).unwrap();

        // Пустой вебхук отбраковывается ДО чтения и записи.
        let bad = PortalConfig::default();
        assert!(save_portals_to(&path, &[good.clone(), bad]).is_err());
        assert!(!tmp_path_for(&path).exists());

        // Каталог только для чтения: ошибка приходит уже из ФС.
        #[cfg(unix)]
        {
            let ro = dir.path().join("ro");
            std::fs::create_dir(&ro).unwrap();
            let target = ro.join("bitrix.toml");
            save_portals_to(&target, std::slice::from_ref(&good)).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();

            let other = PortalConfig::from_webhook(
                &WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/"),
                None,
            );
            assert!(save_portals_to(&target, &[other]).is_err());
            assert!(!tmp_path_for(&target).exists(), "tmp остался после ошибки");

            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
            // Целевой файл остался прежним.
            let entries: Vec<String> = std::fs::read_dir(&ro)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            assert_eq!(entries, vec!["bitrix.toml".to_string()], "{entries:?}");
            assert_eq!(
                BitrixRootConfig::load_from_file(&target).portals[0]
                    .webhook
                    .expose(),
                good.webhook.expose()
            );
        }

        // Первый портал на месте и невредим.
        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme"]);
        assert_eq!(root.portals[0].webhook.expose(), good.webhook.expose());
    }

    /// Удаление одного портала оставляет остальные и идемпотентно.
    #[test]
    fn removing_one_portal_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        add_portal_to(
            &path,
            &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/"),
            None,
        )
        .unwrap();
        add_portal_to(
            &path,
            &WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/"),
            None,
        )
        .unwrap();

        assert!(remove_portal_from(&path, "acme").unwrap());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("aaa111aaa111"), "{body}");
        assert!(body.contains("bbb222bbb222"), "{body}");
        assert_eq!(BitrixRootConfig::load_from_file(&path).portal_ids(), vec!["beta"]);

        // Повторное удаление — не ошибка.
        assert!(!remove_portal_from(&path, "acme").unwrap());
        assert!(!remove_portal_from(&dir.path().join("nope.toml"), "acme").unwrap());
    }

    /// Перевыпущенный вебхук меняет ТОЛЬКО секрет своего портала: `id`, имя,
    /// место в списке и секреты соседей целы.
    #[test]
    fn replacing_a_webhook_keeps_the_identity_the_place_and_the_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        add_portal_to(
            &path,
            &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111aaa111/"),
            Some("Рабочий"),
        )
        .unwrap();
        add_portal_to(
            &path,
            &WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/"),
            None,
        )
        .unwrap();

        let reissued = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/ccc333ccc333/");
        assert!(replace_portal_webhook_in(&path, "acme", &reissued).unwrap());

        let root = BitrixRootConfig::load_from_file(&path);
        assert_eq!(root.portal_ids(), vec!["acme", "beta"], "порядок сбит");
        assert_eq!(root.portals[0].title, "Рабочий", "имя потеряно");
        assert_eq!(root.portals[0].webhook.expose(), reissued.expose());
        assert_eq!(
            root.portals[1].webhook.expose(),
            "https://beta.bitrix24.ru/rest/9/bbb222bbb222/",
            "тронут секрет соседа"
        );

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("aaa111aaa111"), "старый секрет остался: {body}");

        // Незнакомый портал — не ошибка и не повод переписывать файл.
        assert!(!replace_portal_webhook_in(&path, "nope", &reissued).unwrap());
        // Пустой вебхук — отказ: «отвязать» умеет remove_portal_from.
        assert!(replace_portal_webhook_in(&path, "acme", &WebhookUrl::default()).is_err());
    }

    // -------------------------------------------------------------------
    // Миграция legacy-конфига (Ф1.4).
    // -------------------------------------------------------------------

    /// Legacy-файл читается как ОДИН портал, и чтение ничего не переписывает.
    #[test]
    fn a_legacy_config_reads_as_a_single_portal_without_rewriting_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let body = "webhook = \"https://legacy.bitrix24.ru/rest/1/aaa111aaa111/\"\npoll_active_secs = 5\n";
        std::fs::write(&path, body).unwrap();

        let root = BitrixRootConfig::load_from_file(&path);
        assert!(root.migrated_from_legacy);
        assert_eq!(root.portal_ids(), vec!["legacy"]);
        assert_eq!(root.shared.poll_active_secs, 5);

        let cfg = BitrixConfig::load_from_file(&path);
        assert!(cfg.has_webhook(), "поведение плагина обязано не измениться");
        assert_eq!(
            cfg.webhook.expose(),
            "https://legacy.bitrix24.ru/rest/1/aaa111aaa111/"
        );
        assert_eq!(cfg.portal_id, "legacy");
        assert_eq!(cfg.poll_active_secs, 5);

        // ЧТЕНИЕ НЕРАЗРУШАЮЩЕЕ: файл байт в байт прежний.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    /// Миграция происходит при первой ЗАПИСИ, идемпотентна, и секрет после неё
    /// лежит в файле ровно один раз.
    #[test]
    fn the_first_write_migrates_the_legacy_key_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        std::fs::write(
            &path,
            "webhook = \"https://legacy.bitrix24.ru/rest/1/aaa111aaa111/\"\nkeep_me = true\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // Любая запись мигрирует: добавляем ВТОРОЙ портал.
        let added = add_portal_to(
            &path,
            &WebhookUrl::new("https://beta.bitrix24.ru/rest/9/bbb222bbb222/"),
            None,
        )
        .unwrap();
        assert_eq!(added.id, "beta");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[[portals]]"), "{body}");
        assert!(body.contains("keep_me = true"), "{body}");
        // Верхнеуровневый ключ снят, а секрет не продублирован.
        assert_eq!(body.matches("aaa111aaa111").count(), 1, "{body}");
        assert!(
            !body
                .lines()
                .any(|l| l.trim_start().starts_with("webhook = \"https://legacy")
                    && !body.contains("[[portals]]")),
            "{body}"
        );
        #[cfg(unix)]
        assert_eq!(mode_of(&path), 0o600);
        assert!(!tmp_path_for(&path).exists());

        let root = BitrixRootConfig::load_from_file(&path);
        assert!(!root.migrated_from_legacy, "второй раз мигрировать нечего");
        assert_eq!(root.portal_ids(), vec!["legacy", "beta"]);

        // Повторная миграция идемпотентна: id портала-мигранта не сменился.
        add_portal_to(
            &path,
            &WebhookUrl::new("https://legacy.bitrix24.ru/rest/1/aaa111aaa111/"),
            None,
        )
        .unwrap();
        assert_eq!(
            BitrixRootConfig::load_from_file(&path).portal_ids(),
            vec!["legacy", "beta"]
        );
    }

    /// Идентификатор портала-мигранта детерминирован: на него завязаны
    /// миграции кэша (Ф6) и настроек (Ф7).
    #[test]
    fn the_migrant_portal_id_is_deterministic() {
        let w = WebhookUrl::new("https://legacy.bitrix24.ru/rest/1/aaa111aaa111/");
        let first = portal_id_from_webhook(&w);
        for _ in 0..3 {
            assert_eq!(portal_id_from_webhook(&w), first);
        }

        let dir = tempfile::tempdir().unwrap();
        for n in 0..3 {
            let path = dir.path().join(format!("bitrix{n}.toml"));
            std::fs::write(&path, format!("webhook = \"{}\"\n", w.expose())).unwrap();
            assert_eq!(BitrixRootConfig::load_from_file(&path).portal_ids(), vec![first.clone()]);
            save_webhook_to(&path, &w).unwrap();
            assert_eq!(BitrixRootConfig::load_from_file(&path).portal_ids(), vec![first.clone()]);
        }
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
