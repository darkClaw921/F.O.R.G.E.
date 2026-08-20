//! Облачный провайдер polza.ai: расшифровка и саммари без локального GPU.
//!
//! Модуль НЕ импортирует `axum` и `http`: он живёт рядом с [`crate::pipeline`],
//! а превращение ошибки в HTTP-ответ остаётся работой [`crate::routes`].
//!
//! # Что здесь есть и почему именно так
//!
//! polza.ai — OpenAI-совместимый агрегатор. Всё, что ниже, снято боевыми
//! запросами (полный отчёт — `.arhit/docs/pages/forge-meet-polza-probe.md`), а
//! не взято из документации:
//!
//! | вызов | зачем |
//! |---|---|
//! | `GET /key` | **бесплатный** — самый дешёвый способ проверить живость ключа и остаток лимита; именно он идёт в `GET /api/meet/status` |
//! | `GET /balance` | остаток в РУБЛЯХ, суммы приходят строками |
//! | `GET /models` | каталог; цены лежат в `top_provider.pricing` |
//! | `POST /audio/transcriptions` | расшифровка окнами ≤ 90 с ([`stt`]) |
//! | `POST /chat/completions` | саммари потоком SSE ([`summary`]) |
//!
//! `/credits`, `/me`, `/auth/key` отвечают 404 — их нет, и пробовать их не надо.
//!
//! # Три решения, которые определяют весь модуль
//!
//! 1. **`base_url` — поле конфига, а не константа.** Это единственная точка
//!    подмены в тестах: локальный стенд `tests/polza_stub.rs` поднимается на
//!    `127.0.0.1:0` и подставляется сюда. Ровно поэтому в крейт НЕ заводится
//!    `wiremock`: он стал бы четвёртым способом делать то же самое, а прямая
//!    dev-зависимость на него сделала бы ложным объяснение в шапке
//!    `Cargo.toml`, по которому `regex` не подключается напрямую именно потому,
//!    что живёт в дереве транзитивом wiremock.
//! 2. **Ключ живёт в [`SecretKey`] и уходит ТОЛЬКО заголовком
//!    `Authorization`.** Ни в argv (виден в `ps aux`), ни в URL (попадает в
//!    логи прокси и в текст ошибок `reqwest`), ни в `Debug` клиента и ошибок.
//!    Ошибки `reqwest` перед показом чистятся `without_url()` — у запроса с
//!    ключом в query это была бы прямая утечка, и запрет «ключ не в URL»
//!    держится не дисциплиной, а тем, что показывать URL здесь просто нечему.
//! 3. **Общего таймаута у HTTP-клиента НЕТ.** `reqwest::ClientBuilder::timeout`
//!    ограничивает запрос ЦЕЛИКОМ, вместе с чтением тела, и убил бы стриминг
//!    саммари на середине длинного ответа. Поэтому здесь только
//!    `connect_timeout`, а потолок ставится на каждый вызов отдельно: у
//!    служебных вызовов — на весь запрос, у стрима — на паузу между кадрами
//!    ([`summary::stream_chat`]).
//!
//! # Зависимости
//!
//! HTTP — только `reqwest` из `[workspace.dependencies]`, откуда
//! `default-features = false` неотменяем. Гейты `cargo tree -d` и
//! `cargo tree -i openssl-sys` обязаны оставаться пустыми; feature `multipart`
//! (нужна `POST /audio/transcriptions`) TLS-бэкенд не трогает и в дерево ничего
//! нового не приносит — её уже включает `forge-vk`.

pub mod service;
pub mod stt;
pub mod summary;

use std::fmt;
use std::time::Duration;

use crate::pipeline::proc::ProcError;

/// База API. Тот же ответ отдают `/v1/...` и `polza.ai/api/v1/...`.
pub const DEFAULT_BASE_URL: &str = "https://api.polza.ai/api/v1";

/// Единственная модель облачного STT, дающая таймкоды.
///
/// Развилки здесь нет: `openai/whisper-large-v3-turbo` быстрее и втрое дешевле
/// (0,048 ₽/мин против 0,432 ₽/мин), но идёт через «OpenRouter Audio API»,
/// который на `verbose_json` отвечает отказом «Допустимые значения: json,
/// text». Без таймкодов расшифровку не на что положить: вся атрибуция спикеров
/// построена на пересечении интервалов.
pub const DEFAULT_STT_MODEL: &str = "openai/whisper-1";

/// Модель саммари по умолчанию.
///
/// Контекст 1 048 576 токенов: часовая встреча (≈ 30 тыс. токенов) помещается
/// целиком, map-reduce не нужен. Константа — только ДЕФОЛТ поля конфига:
/// каталог отдаёт также Claude, GPT-5.x, DeepSeek и YandexGPT, и выбор обязан
/// оставаться за пользователем.
pub const DEFAULT_SUMMARY_MODEL: &str = "google/gemini-3.5-flash";

/// Облачные модели расшифровки, которые ДАЮТ таймкоды.
///
/// Список, а не одна константа: каталог живой, и завтра пригодных моделей
/// может стать две. Знание живёт здесь, в клиенте сервиса, а НЕ во фронте:
/// интерфейс обязан получать флаг «эта модель ломает атрибуцию» готовым
/// полем, иначе он повторит зашитое имя модели третьим экземпляром и
/// разъедется с реальностью молча.
pub const TIMESTAMPED_STT_MODELS: &[&str] = &[DEFAULT_STT_MODEL];

/// Даёт ли облачная модель расшифровки таймкоды.
///
/// `false` означает не «хуже», а «атрибуция спикеров невозможна»: сегменты
/// не на что положить на таймлайне.
#[must_use]
pub fn stt_gives_timestamps(model: &str) -> bool {
    TIMESTAMPED_STT_MODELS.contains(&model.trim())
}

/// Формат ответа STT, при котором приходят таймкоды. **Посегментные.**
pub const STT_RESPONSE_FORMAT: &str = "verbose_json";

/// Потолок окна расшифровки, секунды аудио.
///
/// Предел облака — около 60 секунд ПО СТЕНЕ на запрос: две минуты аудио дают
/// 200 за 35 с, три минуты — обрыв TCP на 62-й секунде. Предел по стене, а не
/// по размеру, поэтому окно меряется длительностью аудио и берётся с запасом.
pub const MAX_STT_WINDOW_SECS: u64 = 90;

/// Окно расшифровки по умолчанию.
pub const DEFAULT_STT_WINDOW_SECS: u64 = 90;

/// Таймаут установления соединения.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Потолок служебного вызова (`/key`, `/balance`, `/models`).
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Потолок одного окна расшифровки.
///
/// Вдвое больше замеренного предела в 60 секунд: собственный таймаут обязан
/// срабатывать ПОЗЖЕ, чем отвечает или рвётся сам сервис, иначе окно, которое
/// вот-вот доедет, снималось бы своими руками.
pub const DEFAULT_STT_TIMEOUT_SECS: u64 = 120;

/// Максимальная пауза между кадрами SSE, прежде чем стрим считается мёртвым.
pub const DEFAULT_STREAM_IDLE_SECS: u64 = 120;

/// Сколько символов чужого тела показывается в ошибке.
const BODY_TAIL_LIMIT: usize = 600;

/// Символ, которым закрашивается ключ в маске.
///
/// Константа, а не литерал в двух местах: по ней же
/// [`crate::config::MeetConfig::apply_editable`] узнаёт присланную обратно
/// маску и отказывается принимать её за ключ.
pub const MASK_CHAR: char = '•';

// ===========================================================================
// Ключ
// ===========================================================================

/// Ключ API, который не печатается.
///
/// `Debug` и `Display` отдают МАСКУ, и это не украшение: структура с ключом
/// внутри рано или поздно попадает в `tracing::debug!("{cfg:?}")` — и утечка
/// случается не в момент написания кода, а через полгода, в чужой правке.
/// Достать значение можно только явным [`SecretKey::expose`], который видно в
/// ревью.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretKey(String);

impl SecretKey {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into().trim().to_string())
    }

    /// Пустой ключ означает «облако не настроено».
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Единственный способ получить значение. Зовётся только там, где ключ
    /// кладётся в заголовок `Authorization`.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Маска для интерфейса: `pza_RIcg••••••••Uzu4js`.
    ///
    /// Форма скопирована у самого polza.ai — поле `label` в ответе `GET /key`
    /// выглядит ровно так, и человек узнаёт свой ключ, не видя его.
    #[must_use]
    pub fn mask(&self) -> String {
        let chars: Vec<char> = self.0.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        // Короткий ключ прячется целиком: показывать половину восьмисимвольной
        // строки — значит показать ключ.
        if chars.len() < 20 {
            return MASK_CHAR.to_string().repeat(chars.len().min(12));
        }
        let head: String = chars[..8].iter().collect();
        let tail: String = chars[chars.len() - 6..].iter().collect();
        let mid: String = MASK_CHAR.to_string().repeat(8);
        format!("{head}{mid}{tail}")
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.mask())
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.mask())
    }
}

// ===========================================================================
// Конфиг
// ===========================================================================

/// Настройки облачного провайдера.
///
/// Отдельная структура, а не поля [`crate::config::MeetConfig`]: она полностью
/// описывает клиент, и тест собирает её из адреса локального стенда, не поднимая
/// весь конфиг плагина. Разбор TOML и переключатели стадий — работа Ф15,
/// которая просто соберёт эту структуру.
#[derive(Debug, Clone)]
pub struct CloudConfig {
    /// База API. **Точка подмены в тестах.**
    pub base_url: String,
    pub api_key: SecretKey,
    /// Модель расшифровки. Годится только та, что даёт таймкоды.
    pub stt_model: String,
    /// Модель саммари.
    pub summary_model: String,
    /// Язык расшифровки (`ru`); пустая строка — не передавать.
    pub stt_language: String,
    /// Окно расшифровки, секунды аудио. Больше [`MAX_STT_WINDOW_SECS`]
    /// обрезается: предел облака измерен, и «сделать окно побольше» означает
    /// потерять окно целиком.
    pub stt_window_secs: u64,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub stt_timeout_secs: u64,
    pub stream_idle_secs: u64,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: SecretKey::default(),
            stt_model: DEFAULT_STT_MODEL.to_string(),
            summary_model: DEFAULT_SUMMARY_MODEL.to_string(),
            stt_language: crate::config::DEFAULT_WHISPER_LANGUAGE.to_string(),
            stt_window_secs: DEFAULT_STT_WINDOW_SECS,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            stt_timeout_secs: DEFAULT_STT_TIMEOUT_SECS,
            stream_idle_secs: DEFAULT_STREAM_IDLE_SECS,
        }
    }
}

impl CloudConfig {
    /// Окно расшифровки в миллисекундах, с обрезкой по измеренному пределу.
    #[must_use]
    pub fn window_ms(&self) -> i64 {
        let secs = self.stt_window_secs.clamp(1, MAX_STT_WINDOW_SECS);
        secs as i64 * 1_000
    }

    /// Настроено ли облако вообще.
    #[must_use]
    pub fn configured(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// ===========================================================================
// Ошибки
// ===========================================================================

/// Отказ облачного вызова.
///
/// Различает ровно то, что различает пользователь: «не настроено», «сеть»,
/// «сервис ответил отказом» и «ответ не разбирается». Ни один вариант не несёт
/// ключа — тела ответа при этом сохраняются, потому что текст отказа
/// («Допустимые значения: json, text») и есть то единственное, что подсказывает
/// человеку, что делать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    /// Ключа нет — облако не настроено.
    NotConfigured,
    /// Сеть, TLS, обрыв соединения.
    Transport(String),
    /// Свой таймаут: ответа не дождались.
    Timeout(String),
    /// Сервис ответил не-2xx.
    Status {
        status: u16,
        /// Машинный код из тела (`UNAUTHORIZED`), если он там был.
        api_code: Option<String>,
        message: String,
    },
    /// Ответ пришёл, но не разбирается.
    Decode(String),
    /// Встречу гасят целиком.
    Cancelled,
}

impl CloudError {
    /// Машиночитаемый код. Фронт различает случаи по нему, а не по тексту.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "cloud_not_configured",
            Self::Transport(_) => "cloud_transport",
            Self::Timeout(_) => "cloud_timeout",
            Self::Status { status, .. } => match status {
                401 | 403 => "cloud_unauthorized",
                402 => "cloud_no_funds",
                429 => "cloud_rate_limited",
                _ => "cloud_http",
            },
            Self::Decode(_) => "cloud_bad_response",
            Self::Cancelled => "cancelled",
        }
    }

    /// Текст для человека.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotConfigured => "облако не настроено: нет ключа polza.ai".to_string(),
            Self::Transport(m) => format!("облако недоступно: {m}"),
            Self::Timeout(m) => format!("облако не ответило: {m}"),
            Self::Status {
                status,
                api_code,
                message,
            } => match api_code {
                Some(c) => format!("polza.ai ответил {status} ({c}): {message}"),
                None => format!("polza.ai ответил {status}: {message}"),
            },
            Self::Decode(m) => format!("ответ polza.ai не разбирается: {m}"),
            Self::Cancelled => "запрос к облаку отменён".to_string(),
        }
    }

    /// Стоит ли повторять запрос. Отказ по ключу и по формату параметров
    /// повторять бессмысленно — повтор потратит ещё одну минуту стены.
    #[must_use]
    pub fn retriable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout(_) => true,
            Self::Status { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// Ошибка `reqwest` без URL: у запроса с ключом в query показ URL был бы
    /// прямой утечкой, и правило держится тем, что URL не показывается вовсе.
    pub(crate) fn from_reqwest(e: reqwest::Error) -> Self {
        let clean = e.without_url();
        if clean.is_timeout() {
            return Self::Timeout(clean.to_string());
        }
        if clean.is_decode() {
            return Self::Decode(clean.to_string());
        }
        Self::Transport(clean.to_string())
    }
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for CloudError {}

/// Облачный отказ встаёт на то же место, что и отказ внешнего процесса:
/// пайплайн и журнал встречи уже умеют показывать [`ProcError`] пользователю.
impl From<CloudError> for ProcError {
    fn from(e: CloudError) -> Self {
        let tail = match &e {
            CloudError::Status { message, .. } => message.clone(),
            _ => String::new(),
        };
        ProcError::new(e.code(), e.message(), tail)
    }
}

// ===========================================================================
// Клиент
// ===========================================================================

/// HTTP-клиент polza.ai.
///
/// Дешёвый для клонирования: внутри `reqwest::Client`, который сам по себе
/// `Arc`. Пул соединений общий — на встрече уходит несколько десятков окон
/// расшифровки подряд, и новое TLS-рукопожатие на каждое стоило бы дороже
/// самого запроса.
#[derive(Clone)]
pub struct CloudClient {
    http: reqwest::Client,
    base_url: String,
    key: SecretKey,
}

/// `Debug` печатает МАСКУ ключа: см. решение 2 в шапке модуля.
impl fmt::Debug for CloudClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudClient")
            .field("base_url", &self.base_url)
            .field("key", &self.key)
            .finish()
    }
}

impl CloudClient {
    /// Собирает клиент. Пустой ключ — это [`CloudError::NotConfigured`], а не
    /// запрос, который отвалится по 401 через полминуты.
    pub fn new(cfg: &CloudConfig) -> Result<Self, CloudError> {
        if !cfg.configured() {
            return Err(CloudError::NotConfigured);
        }
        let http = reqwest::Client::builder()
            // Только соединение. Потолок ответа ставится на каждый вызов: общий
            // таймаут убил бы стриминг саммари (см. решение 3 в шапке).
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs.max(1)))
            .user_agent("forge-meet")
            .build()
            .map_err(CloudError::from_reqwest)?;
        Ok(Self {
            http,
            base_url: normalise_base(&cfg.base_url),
            key: cfg.api_key.clone(),
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Полный адрес вызова.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// GET с ключом в ЗАГОЛОВКЕ.
    pub(crate) fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path)).bearer_auth(self.key.expose())
    }

    /// POST с ключом в ЗАГОЛОВКЕ.
    pub(crate) fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.post(self.url(path)).bearer_auth(self.key.expose())
    }

    /// Отправляет запрос и разбирает тело как JSON.
    ///
    /// Не-2xx превращается в [`CloudError::Status`] С ТЕЛОМ: текст отказа
    /// polza.ai — единственное, что объясняет человеку причину.
    pub(crate) async fn send_json(
        &self,
        rb: reqwest::RequestBuilder,
        timeout: Duration,
    ) -> Result<serde_json::Value, CloudError> {
        let resp = rb
            .timeout(timeout)
            .send()
            .await
            .map_err(CloudError::from_reqwest)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(CloudError::from_reqwest)?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &body));
        }
        serde_json::from_str(&body).map_err(|e| CloudError::Decode(format!("{e}: {}", tail(&body))))
    }
}

/// Приводит базу к виду без завершающего слэша.
fn normalise_base(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// Обрезает чужое тело до разумного хвоста для сообщения об ошибке.
fn tail(body: &str) -> String {
    let t = body.trim();
    if t.chars().count() <= BODY_TAIL_LIMIT {
        return t.to_string();
    }
    let cut: String = t.chars().take(BODY_TAIL_LIMIT).collect();
    format!("{cut}…")
}

/// Разбирает тело отказа.
///
/// Живая форма: `{"error":{"code":"UNAUTHORIZED","message":"…","trace_id":"…"}}`
/// при HTTP 401. Но полагаться на неё нельзя — перед сервисом стоит шлюз, и
/// 502 приходит HTML-страницей, поэтому неразобранное тело показывается как
/// есть.
pub(crate) fn status_error(status: u16, body: &str) -> CloudError {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let node = parsed.as_ref().and_then(|v| v.get("error"));
    let api_code = node
        .and_then(|e| e.get("code"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let message = node
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| tail(body));
    CloudError::Status {
        status,
        api_code,
        message,
    }
}

/// Число из значения, которое может прийти И строкой, И числом.
///
/// Суммы баланса и цены polza.ai отдаёт СТРОКАМИ (`"461.88706171"`), а лимиты
/// ключа — числами. Один разбор на оба случая: иначе первая же смена формы на
/// их стороне превратилась бы в «баланс 0».
#[must_use]
pub fn number(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Строка из значения.
#[must_use]
pub fn text(v: Option<&serde_json::Value>) -> Option<String> {
    v?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_never_prints_itself() {
        let k = SecretKey::new("pza_TESTnotArealKeyJustAFixture_mask01");
        let shown = format!("{k:?} {k}");
        assert!(
            !shown.contains("notArealKeyJustAFixture"),
            "ключ утёк в вывод: {shown}"
        );
        assert!(shown.contains("pza_TEST"), "{shown}");
        assert!(shown.contains("mask01"), "{shown}");
        assert_eq!(k.expose(), "pza_TESTnotArealKeyJustAFixture_mask01");

        // Короткий ключ прячется целиком: у него нет «безопасной» половины.
        let short = SecretKey::new("pza_abc");
        assert_eq!(short.mask(), "•••••••");
        assert!(!format!("{short:?}").contains("abc"));
        assert!(SecretKey::new("   ").is_empty(), "пробелы — не ключ");
    }

    #[test]
    fn the_client_debug_shows_only_the_mask() {
        let cfg = CloudConfig {
            api_key: SecretKey::new("pza_TESTnotArealKeyJustAFixture_mask01"),
            base_url: "http://127.0.0.1:9/api/v1".into(),
            ..CloudConfig::default()
        };
        let c = CloudClient::new(&cfg).unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("notArealKeyJustAFixture"), "{shown}");
        assert!(shown.contains("pza_TEST••••••••mask01"), "{shown}");
        // И конфиг тоже: он попадает в логи целиком куда чаще клиента.
        assert!(!format!("{cfg:?}").contains("notArealKeyJustAFixture"));
    }

    #[test]
    fn an_empty_key_is_not_configured() {
        let cfg = CloudConfig::default();
        assert!(!cfg.configured());
        assert_eq!(CloudClient::new(&cfg).unwrap_err(), CloudError::NotConfigured);
    }

    #[test]
    fn the_base_url_is_normalised_and_paths_join_cleanly() {
        let cfg = CloudConfig {
            api_key: SecretKey::new("pza_key_that_is_long_enough_to_mask"),
            base_url: "http://127.0.0.1:1234/api/v1/".into(),
            ..CloudConfig::default()
        };
        let c = CloudClient::new(&cfg).unwrap();
        assert_eq!(c.base_url(), "http://127.0.0.1:1234/api/v1");
        assert_eq!(c.url("key"), "http://127.0.0.1:1234/api/v1/key");
        assert_eq!(
            c.url("/audio/transcriptions"),
            "http://127.0.0.1:1234/api/v1/audio/transcriptions"
        );
    }

    /// Живая форма отказа: HTTP 401 и `{"error":{"code":…}}`.
    #[test]
    fn the_error_body_is_parsed_into_a_code_and_a_message() {
        let e = status_error(
            401,
            r#"{"error":{"code":"UNAUTHORIZED","message":"Неверные учётные данные авторизации","trace_id":"e93716ed"}}"#,
        );
        assert_eq!(e.code(), "cloud_unauthorized");
        assert!(e.message().contains("Неверные учётные данные"), "{e}");
        assert!(!e.retriable(), "повторять запрос с плохим ключом незачем");

        // Шлюз отвечает HTML — тело показывается как есть, а не теряется.
        let gw = status_error(502, "<html>502 Bad Gateway</html>");
        assert_eq!(gw.code(), "cloud_http");
        assert!(gw.message().contains("502 Bad Gateway"), "{gw}");
        assert!(gw.retriable());
        assert!(status_error(429, "{}").retriable());
    }

    #[test]
    fn error_codes_cover_every_case() {
        assert_eq!(CloudError::NotConfigured.code(), "cloud_not_configured");
        assert_eq!(CloudError::Transport("x".into()).code(), "cloud_transport");
        assert_eq!(CloudError::Timeout("x".into()).code(), "cloud_timeout");
        assert_eq!(CloudError::Decode("x".into()).code(), "cloud_bad_response");
        assert_eq!(CloudError::Cancelled.code(), "cancelled");
        // Отказ облака приезжает пользователю тем же путём, что и отказ
        // внешнего процесса.
        let p: ProcError = CloudError::Status {
            status: 500,
            api_code: None,
            message: "internal".into(),
        }
        .into();
        assert_eq!(p.code, "cloud_http");
        assert!(p.full_message().contains("internal"));
    }

    /// Числа приходят и строками, и числами — разбор один.
    #[test]
    fn numbers_are_read_from_both_strings_and_numbers() {
        let v: serde_json::Value = serde_json::json!({
            "amount": "461.88706171",
            "limit_remaining": 288.7275241,
            "nothing": null,
            "wrong": []
        });
        assert_eq!(number(v.get("amount")), Some(461.88706171));
        assert_eq!(number(v.get("limit_remaining")), Some(288.7275241));
        assert_eq!(number(v.get("nothing")), None);
        assert_eq!(number(v.get("wrong")), None);
        assert_eq!(number(v.get("missing")), None);
    }

    #[test]
    fn the_window_is_clamped_to_the_measured_limit() {
        let cfg = CloudConfig {
            stt_window_secs: 600,
            ..CloudConfig::default()
        };
        assert_eq!(cfg.window_ms(), MAX_STT_WINDOW_SECS as i64 * 1_000);
        assert_eq!(CloudConfig::default().window_ms(), 90_000);
        assert_eq!(
            CloudConfig {
                stt_window_secs: 0,
                ..CloudConfig::default()
            }
            .window_ms(),
            1_000,
            "нулевое окно означало бы бесконечную нарезку"
        );
    }

    /// Длинное чужое тело в сообщение не влезает целиком.
    #[test]
    fn a_huge_body_is_cut_to_a_tail() {
        let body = "я".repeat(5_000);
        let e = status_error(500, &body);
        assert!(e.message().chars().count() < BODY_TAIL_LIMIT + 120, "{e}");
        assert!(e.message().ends_with('…'), "{e}");
    }
}
