//! HTTP-клиент VK: единственная точка выхода плагина в сеть.
//!
//! ```text
//! methods.rs  →  VkClient::call  →  throttle  →  POST form  →  parse_envelope
//!                                                   ↑              ↓
//!                                            ретрай-политика   ConnHealth
//! ```
//!
//! Модуль отвечает за четыре вещи и ни за что больше:
//!
//! 1. **Подстановка секрета.** `access_token` добавляется в параметры ТОЛЬКО
//!    внутри [`VkClient::post_once`], наружу не отдаётся и в `Debug` не
//!    попадает. Токен едет в ТЕЛЕ запроса (form-urlencoded), а не в query —
//!    иначе он оседал бы в логах прокси и в тексте ошибок `reqwest`, которые
//!    цитируют URL.
//! 2. **Соблюдение лимита.** Один [`Throttle`] на клиент, то есть на токен
//!    (см. [`throttle`]).
//! 3. **Ретраи.** По таблице [`error::retry_for`] — как правило не более
//!    одного повтора, но код 6 повторяется до [`error::MAX_CODE6_RETRIES`]
//!    раз (forge-zzy7: одного повтора не хватало под стартовым залпом
//!    собственных вызовов). Повтор НЕ пересобирает параметры — благодаря
//!    этому `messages.send` остаётся идемпотентным по `random_id`.
//! 4. **Здоровье соединения.** Успех ⇒ [`ConnHealth::Ok`], транспортный отказ
//!    ⇒ [`ConnHealth::Down`]. У VK нет постоянного соединения, и без этой
//!    отметки ответ `503 not_connected` был бы ни на чём не основан.
//!
//! # Что НЕ проходит через троттлер
//!
//! [`VkClient::upload_multipart`] (чужой хост, не метод API),
//! [`VkClient::get_stream`] (CDN VK) и [`VkClient::longpoll_check`] (ожидание
//! на LongPoll-сервере: 25 секунд удержания токена съели бы весь бюджет).
//!
//! # Таймауты
//!
//! Клиентский таймаут `reqwest` считается «от начала соединения до конца
//! чтения тела», поэтому один общий на всё не годится:
//!
//! | операция | таймаут | почему |
//! |---|---|---|
//! | метод API | [`HTTP_TIMEOUT`] (30 с) | ответ VK либо приходит быстро, либо не приходит |
//! | connect | [`CONNECT_TIMEOUT`] (10 с) | отличает «нет сети» от «VK думает» |
//! | `a_check` | `wait + `[`LONGPOLL_TIMEOUT_SLACK`] | штатное 25-секундное ожидание иначе зарезал бы дефолт |
//! | скачивание вложения | [`DOWNLOAD_TIMEOUT`] | файл на 100 МБ по мобильной сети законно едет дольше 30 с |
//! | загрузка файла | [`UPLOAD_TIMEOUT`] | предел `max_upload_bytes` — 2 GiB |

pub mod error;
pub mod methods;
pub mod schema;
pub mod throttle;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::RwLock;

pub use error::{
    classify, parse_envelope, parse_upload_response, retry_for, to_api_error, Retry, VkError,
};
pub use throttle::{Bucket, Clock, SystemClock, Throttle, ThrottleStats};

use crate::config::{SecretToken, VkConfig};
use crate::state::ConnHealth;

/// Параметры метода VK.
///
/// Именно `Vec` пар, а не `HashMap`: порядок параметров попадает в тело
/// запроса, а стабильный порядок делает диффы тестов читаемыми. Ключи —
/// `&'static str`, потому что все они литералы: динамическое имя параметра у
/// VK означало бы опечатку.
///
/// `access_token` в `Params` НЕ КЛАДЁТСЯ НИКОГДА — его добавляет
/// [`VkClient::post_once`]. Поэтому отладочная печать `Params` безопасна, и на
/// это есть тест в [`methods`].
pub type Params = Vec<(&'static str, String)>;

/// Таймаут обычного вызова метода.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Таймаут установления соединения.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Запас к `wait` при ожидании на LongPoll-сервере.
pub const LONGPOLL_TIMEOUT_SLACK: u64 = 10;

/// Таймаут скачивания вложения.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Таймаут загрузки файла на upload-сервер.
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(3600);

/// `User-Agent` плагина.
///
/// Честный, а не маскирующийся под официальный клиент: подделка UA не даёт
/// доступа к чему-либо, зато делает трафик неотличимым от чужого при разборе
/// инцидентов.
pub const USER_AGENT: &str = concat!("devforge-vk/", env!("CARGO_PKG_VERSION"));

/// Одна часть multipart-запроса к upload-серверу VK.
///
/// Отдельная структура, а не пять аргументов: у альбома частей до пяти, и
/// позиционный вызов на пятнадцать значений нечитаем. Тело — уже готовое
/// `reqwest::Body` (обычно `Body::wrap_stream(ReaderStream::new(reader))`),
/// потому что вызывающий сам решает, что за поток он оборачивает: у нас это
/// [`crate::progress::ProgressReader`] поверх staging-файла.
pub struct UploadPart {
    /// Имя поля формы: `photo` у одиночного фото, `file1..file5` у альбома,
    /// `file` у документа. Названия диктует VK.
    pub field: String,
    /// Имя файла, которое увидит получатель.
    pub file_name: String,
    /// MIME части.
    pub mime: String,
    /// Точный размер тела — попадёт в `Content-Length` части.
    pub size: u64,
    /// Тело части.
    pub body: reqwest::Body,
}

impl std::fmt::Debug for UploadPart {
    /// Тело печатать нечем (это поток), поэтому в выводе только метаданные.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadPart")
            .field("field", &self.field)
            .field("file_name", &self.file_name)
            .field("mime", &self.mime)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Клиент api.vk.com.
///
/// Дешёвый для клонирования: `reqwest::Client` внутри — это `Arc`. Тем не
/// менее клиент создаётся ОДИН на токен, потому что вместе с ним создаётся и
/// троттлер, а два троттлера означали бы двойной бюджет запросов.
pub struct VkClient {
    http: reqwest::Client,
    base_url: String,
    token: SecretToken,
    api_version: String,
    throttle: Arc<Throttle>,
    health: Arc<RwLock<ConnHealth>>,
    longpoll_wait_secs: u64,
    network_backoff: Duration,
    flood_retry_delay: Duration,
}

impl std::fmt::Debug for VkClient {
    /// Печатает всё, кроме секрета: [`SecretToken`] и так редактируется, но
    /// ручной `Debug` фиксирует это намерение явно и не даст протащить токен
    /// новым полем.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkClient")
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl VkClient {
    /// Собирает клиент из конфигурации.
    ///
    /// `health` приходит снаружи (обычно [`crate::state::VkState::health_handle`]):
    /// клиент и состояние обязаны смотреть на ОДНУ ячейку здоровья, иначе
    /// data-роуты отдавали бы `503` по собственной копии, ничего не знающей о
    /// реальных вызовах.
    ///
    /// # Errors
    ///
    /// Только невозможность собрать `reqwest::Client` (нерабочий TLS-бэкенд,
    /// исчерпание дескрипторов) — конфиг к этому моменту уже проверен
    /// [`VkConfig::validate`].
    pub fn new(cfg: &VkConfig, health: Arc<RwLock<ConnHealth>>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            token: cfg.access_token.clone(),
            api_version: cfg.api_version.clone(),
            throttle: Arc::new(Throttle::cold(f64::from(cfg.rate_limit_rps))),
            health,
            longpoll_wait_secs: cfg.longpoll_wait_secs,
            network_backoff: error::NETWORK_BACKOFF,
            flood_retry_delay: error::FLOOD_RETRY_DELAY,
        })
    }

    /// Задаёт паузы перед повтором.
    ///
    /// Данные, а не константы, ровно по той же причине, что и тайминги WS в
    /// [`crate::state::VkState::with_ws_timings`]: иначе тест «код 6
    /// повторяется один раз» обязан был бы спать настоящую секунду, а тест
    /// транспортного повтора — 500 мс с джиттером. Продовый путь всегда идёт
    /// через [`VkClient::new`], где стоят боевые значения.
    pub fn with_retry_delays(mut self, network_backoff: Duration, flood: Duration) -> Self {
        self.network_backoff = network_backoff;
        self.flood_retry_delay = flood;
        self
    }

    /// Счётчики троттлера — для `GET /api/vk/status` и логов.
    pub async fn throttle_stats(&self) -> ThrottleStats {
        self.throttle.stats().await
    }

    /// Общий троттлер клиента.
    ///
    /// Отдаётся наружу, чтобы LongPoll-воркер (Ф5) мог явно НЕ использовать
    /// его для `a_check`, но использовать для обогащающего
    /// `messages.getLongPollHistory`.
    pub fn throttle(&self) -> Arc<Throttle> {
        self.throttle.clone()
    }

    /// Штатное время ожидания LongPoll из конфига.
    pub fn longpoll_wait_secs(&self) -> u64 {
        self.longpoll_wait_secs
    }

    // -----------------------------------------------------------------
    // Вызов метода.
    // -----------------------------------------------------------------

    /// Вызов метода с разбором ответа в типизированную структуру.
    ///
    /// # Errors
    ///
    /// Всё, что может вернуть [`VkClient::call_value`], плюс [`VkError::Decode`]
    /// при несоответствии ответа ожидаемой форме.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Params,
    ) -> Result<T, VkError> {
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value).map_err(|e| VkError::Decode {
            reason: format!("{method}: {e}"),
        })
    }

    /// Вызов метода с сырым `response` в результате.
    ///
    /// Последовательность: троттлер → `POST` формы → [`parse_envelope`] →
    /// ретрай-политика.
    ///
    /// **Повтор использует ТЕ ЖЕ `params`.** Это не оптимизация, а условие
    /// идемпотентности: VK дедуплицирует `messages.send` по `random_id`, и
    /// пересборка параметров (то есть новый `random_id`) превратила бы
    /// безобидный повтор после обрыва в дубль сообщения у собеседника.
    ///
    /// **Сколько раз повторять — зависит от класса.**
    /// [`Retry::AfterNetworkBackoff`] — не более одного раза, как и раньше.
    /// [`Retry::AfterOneSecond`] (код 6) — до [`error::MAX_CODE6_RETRIES`] раз
    /// (forge-zzy7): см. документацию [`Retry::AfterOneSecond`] о том, почему
    /// одного повтора недостаточно под стартовым залпом собственных вызовов.
    ///
    /// # Errors
    ///
    /// [`VkError::Transport`] / [`VkError::Http`] — до VK не доехали;
    /// [`VkError::Decode`] — ответ не разобрался; [`VkError::Api`] — VK
    /// отказал по существу (напоминаем: с HTTP 200).
    pub async fn call_value(&self, method: &str, params: Params) -> Result<Value, VkError> {
        let mut attempt: u32 = 0;
        loop {
            self.throttle.acquire().await;
            let started = Instant::now();
            let outcome = self.post_once(method, &params).await;
            let elapsed_ms = started.elapsed().as_millis() as u64;

            let err = match outcome {
                Ok(value) => {
                    self.note_ok().await;
                    tracing::debug!(
                        target: "forge_vk",
                        method,
                        elapsed_ms,
                        attempt,
                        "forge-vk: vk call succeeded"
                    );
                    return Ok(value);
                }
                Err(e) => e,
            };

            if err.is_transport() {
                self.note_down(err.to_string()).await;
            }

            let retry = retry_for(&err);
            // Код 6 получает больше попыток, чем всё остальное — см.
            // документацию `Retry::AfterOneSecond` (forge-zzy7).
            let may_retry = match retry {
                Retry::Never => false,
                Retry::AfterNetworkBackoff => attempt == 0,
                Retry::AfterOneSecond => attempt < error::MAX_CODE6_RETRIES,
            };
            if may_retry {
                let delay = self.retry_delay(retry, attempt);
                // В лог уходят метод, длительность и КОД ошибки — но никогда
                // параметры: в них живёт токен.
                tracing::warn!(
                    target: "forge_vk",
                    method,
                    elapsed_ms,
                    attempt,
                    error = %err,
                    retry_in_ms = delay.as_millis() as u64,
                    "forge-vk: vk call failed, retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }

            if retry == Retry::AfterOneSecond {
                // Код 6 пережил все положенные повторы — либо стартовый залп
                // оказался ещё гуще обычного, либо `rate_limit_rps` и правда
                // завышен. Дальше это уже честный 429 пользователю.
                tracing::warn!(
                    target: "forge_vk",
                    method,
                    attempts = attempt + 1,
                    "forge-vk: VK reports `too many requests` even after retrying — lower rate_limit_rps"
                );
            }
            tracing::warn!(
                target: "forge_vk",
                method,
                elapsed_ms,
                attempt,
                error = %err,
                "forge-vk: vk call failed"
            );
            return Err(err);
        }
    }

    /// Одна попытка: собрать форму, отправить, разобрать конверт.
    ///
    /// ЕДИНСТВЕННОЕ место, где `access_token` попадает в запрос.
    async fn post_once(&self, method: &str, params: &Params) -> Result<Value, VkError> {
        let url = format!("{}/{}", self.base_url, method);

        // Форма собирается заново на каждую попытку, но ИЗ ТЕХ ЖЕ значений:
        // повтор обязан быть побайтово тем же запросом (см. `random_id`).
        let mut form: Vec<(&str, &str)> = Vec::with_capacity(params.len() + 2);
        for (k, v) in params {
            form.push((k, v.as_str()));
        }
        form.push(("access_token", self.token.expose()));
        form.push(("v", self.api_version.as_str()));

        let resp = self
            .http
            .post(&url)
            .form(&form)
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(transport_error)?;
        parse_envelope(status, &body)
    }

    /// Пауза перед повтором номер `attempt` (нумерация с нуля).
    ///
    /// Джиттер нужен, чтобы N параллельных запросов, упавших от одного
    /// обрыва, не пошли на повтор в одну и ту же миллисекунду и не устроили
    /// второй залп по едва поднявшемуся серверу.
    ///
    /// Код 6 ждёт по удвоенной лестнице (1 с, затем 2 с), а не одну секунду
    /// каждый раз. Причина не в вежливости: повтор шлёт ТОТ ЖЕ метод с ТЕМИ
    /// ЖЕ параметрами, а однотипные частые запросы VK эскалирует из кода 6
    /// («часто», отпускает за секунду) в код 9 («flood control», держит
    /// минутами и повторам не поддаётся вовсе). Две попытки через равную
    /// секунду — ровно тот профиль, который VK и считает флудом.
    fn retry_delay(&self, retry: Retry, attempt: u32) -> Duration {
        match retry {
            Retry::Never => Duration::ZERO,
            Retry::AfterOneSecond => {
                let factor = 1u32 << attempt.min(error::MAX_CODE6_RETRIES);
                self.flood_retry_delay * factor
            }
            Retry::AfterNetworkBackoff => {
                self.network_backoff + Duration::from_millis(jitter_ms(error::NETWORK_BACKOFF_JITTER_MS))
            }
        }
    }

    // -----------------------------------------------------------------
    // Операции ВНЕ троттлера.
    // -----------------------------------------------------------------

    /// Ожидание событий на LongPoll-сервере (`act=a_check`).
    ///
    /// НЕ троттлится: это не метод API, а ожидание на чужом хосте. Удерживать
    /// на нём токен 25 секунд означало бы отдать весь бюджет запросов одному
    /// вызову.
    ///
    /// `server` приходит от `messages.getLongPollServer` БЕЗ схемы
    /// (`lp.vk.com/wh123456`) — она дописывается здесь. Значение со схемой
    /// используется как есть: так локальный стенд (`tests/vk_stub.rs`)
    /// подставляет свой `http://127.0.0.1:PORT/lp`, не заводя TLS.
    ///
    /// Возвращает СЫРОЙ JSON: у LongPoll свой протокол
    /// (`{"ts":…,"updates":[…]}` либо `{"failed":N}`), а не конверт API.
    /// Разбор — работа [`crate::longpoll::parse_poll`].
    ///
    /// `mode` приходит ПАРАМЕТРОМ, а не константой транспорта, потому что это
    /// решение уровня протокола событий, а не уровня HTTP: боевое значение
    /// [`crate::longpoll::LP_MODE`] обязано нести бит 32 (`pts`), иначе
    /// `messages.getLongPollHistory` вызвать нечем и обогащение пачки одним
    /// запросом деградирует до запроса на каждое сообщение.
    ///
    /// # Errors
    ///
    /// [`VkError::Transport`] при обрыве/таймауте, [`VkError::Http`] при
    /// статусе не 200, [`VkError::Decode`] если ответ не JSON.
    pub async fn longpoll_check(
        &self,
        server: &str,
        key: &str,
        ts: u64,
        wait_secs: u64,
        mode: u32,
    ) -> Result<Value, VkError> {
        let url = if server.starts_with("http://") || server.starts_with("https://") {
            server.to_string()
        } else {
            format!("https://{server}")
        };
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("act", "a_check".to_string()),
                ("key", key.to_string()),
                ("ts", ts.to_string()),
                ("wait", wait_secs.to_string()),
                ("mode", mode.to_string()),
                ("version", methods::LP_VERSION.to_string()),
            ])
            // Штатное ожидание — `wait` секунд; дефолтные 30 зарезали бы его.
            .timeout(Duration::from_secs(wait_secs + LONGPOLL_TIMEOUT_SLACK))
            .send()
            .await
            .map_err(transport_error)?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(VkError::Http { status });
        }
        let body = resp.bytes().await.map_err(transport_error)?;
        serde_json::from_slice(&body).map_err(|e| VkError::Decode {
            reason: format!("longpoll: {e}"),
        })
    }

    /// Заливает файл на upload-сервер VK.
    ///
    /// НЕ троттлится (чужой хост) и НЕ буферизует файл в памяти: тело едет
    /// потоком, а `stream_with_length` проставляет `Content-Length` — без него
    /// `reqwest` перешёл бы на chunked, который upload-сервер VK не принимает.
    ///
    /// Ответ upload-сервера — голый объект без конверта `response`, поэтому
    /// разбирается [`parse_upload_response`], а не [`parse_envelope`].
    ///
    /// # Errors
    ///
    /// Транспортные отказы, статус не 200, нераспознанное тело, а также
    /// `{"error":…}` от самого upload-сервера.
    pub async fn upload_multipart<S>(
        &self,
        upload_url: &str,
        field: &str,
        file_name: &str,
        size: u64,
        stream: S,
    ) -> Result<Value, VkError>
    where
        S: futures_util::TryStream + Send + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        bytes::Bytes: From<S::Ok>,
    {
        let mime = mime_guess::from_path(file_name)
            .first_or_octet_stream()
            .to_string();
        self.upload_multipart_parts(
            upload_url,
            vec![UploadPart {
                field: field.to_string(),
                file_name: file_name.to_string(),
                mime,
                size,
                body: reqwest::Body::wrap_stream(stream),
            }],
        )
        .await
    }

    /// Заливает НЕСКОЛЬКО файлов одним multipart-запросом.
    ///
    /// Существует ради альбомов: VK принимает до пяти фотографий полями
    /// `file1..file5` и сохраняет их ОДНИМ `photos.saveMessagesPhoto`. Пятёрка
    /// по одному стоила бы 15 троттленных запросов (три на файл) и пять секунд
    /// при лимите 3 rps; одним запросом — те же три и около секунды.
    ///
    /// Правила те же, что у [`VkClient::upload_multipart`]: не троттлится,
    /// каждая часть едет потоком с объявленной длиной
    /// (`stream_with_length`), ответ разбирается [`parse_upload_response`].
    ///
    /// # Errors
    ///
    /// Транспортные отказы, статус не 200, нераспознанное тело, `{"error":…}`
    /// от upload-сервера, а также [`VkError::Decode`] на невалидном MIME части.
    pub async fn upload_multipart_parts(
        &self,
        upload_url: &str,
        parts: Vec<UploadPart>,
    ) -> Result<Value, VkError> {
        let mut form = reqwest::multipart::Form::new();
        for p in parts {
            // ↓ stream_with_length ОБЯЗАТЕЛЕН: с `stream()` reqwest не знает
            //   длину, уходит в chunked и буферизует файл целиком — отправка
            //   полугигабайтного видео уехала бы в RSS процесса.
            let part = reqwest::multipart::Part::stream_with_length(p.body, p.size)
                .file_name(p.file_name)
                .mime_str(&p.mime)
                .map_err(|e| VkError::Decode {
                    reason: format!("bad mime `{}`: {e}", p.mime),
                })?;
            form = form.part(p.field, part);
        }

        let resp = self
            .http
            .post(upload_url)
            .multipart(form)
            // Загрузка двухгигабайтного файла законно длится дольше 30 секунд.
            .timeout(UPLOAD_TIMEOUT)
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(transport_error)?;
        parse_upload_response(status, &body)
    }

    /// Открывает поток байтов вложения.
    ///
    /// НЕ троттлится: это CDN VK, а не API. Тело НЕ вычитывается — вызывающий
    /// (Ф7) перекладывает его в файл кэша потоком, чтобы стомегабайтное видео
    /// не оказалось в памяти процесса.
    ///
    /// # Errors
    ///
    /// Транспортные отказы и статус не 2xx (протухшая подписанная ссылка VK
    /// отвечает `403`).
    pub async fn get_stream(&self, url: &str) -> Result<reqwest::Response, VkError> {
        let resp = self
            .http
            .get(url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(transport_error)?;
        if !resp.status().is_success() {
            return Err(VkError::Http {
                status: resp.status().as_u16(),
            });
        }
        Ok(resp)
    }

    // -----------------------------------------------------------------
    // Здоровье соединения.
    // -----------------------------------------------------------------

    /// Отмечает успешный вызов.
    async fn note_ok(&self) {
        *self.health.write().await = ConnHealth::Ok {
            at: chrono::Utc::now().timestamp(),
        };
    }

    /// Отмечает транспортный отказ.
    ///
    /// Повторный отказ НЕ сдвигает `since` — иначе череда сбоев продлевала бы
    /// окно `503` бесконечно, и здоровье уже никогда не смогло бы починиться
    /// (правило дословно повторяет [`crate::state::VkState::note_down`]).
    async fn note_down(&self, reason: impl Into<String>) {
        let mut guard = self.health.write().await;
        if matches!(&*guard, ConnHealth::Down { .. }) {
            return;
        }
        *guard = ConnHealth::Down {
            since: chrono::Utc::now().timestamp(),
            reason: reason.into(),
        };
    }
}

/// Превращает ошибку `reqwest` в [`VkError::Transport`].
///
/// `without_url()` обязателен: текст ошибки `reqwest` цитирует URL, а у
/// LongPoll в query лежит `key` — производный от токена секрет. Метод API в
/// URL не секретен, но правило «URL в текст ошибки не попадает» дешевле
/// держать одним, чем помнить, где можно.
fn transport_error(e: reqwest::Error) -> VkError {
    VkError::Transport {
        reason: e.without_url().to_string(),
    }
}

/// Псевдослучайные миллисекунды `0..=max` без крейта `rand`.
///
/// Источник — UUID v4 (крейт уже в дереве ради `upload_id` и `random_id`).
/// Криптографического качества здесь не нужно: джиттер лишь разводит
/// одновременные повторы.
fn jitter_ms(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    (uuid::Uuid::new_v4().as_u128() as u64) % (max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VkConfig {
        VkConfig {
            access_token: SecretToken::new("vk1.a.UNIT_TEST_TOKEN_0123456789"),
            base_url: "https://api.vk.com/method".into(),
            ..VkConfig::default()
        }
    }

    fn client() -> VkClient {
        VkClient::new(&cfg(), Arc::new(RwLock::new(ConnHealth::default()))).unwrap()
    }

    /// `Debug` клиента не должен раскрывать токен — ни целиком, ни отпечатком.
    #[test]
    fn debug_never_leaks_the_token() {
        let dbg = format!("{:?}", client());
        assert!(!dbg.contains("UNIT_TEST_TOKEN"), "{dbg}");
        assert!(!dbg.contains("0123456789"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert!(dbg.contains("api.vk.com"), "адрес — не секрет: {dbg}");
    }

    /// Хвостовой слэш в `base_url` не должен давать `//method`: VK отвечает на
    /// такой путь HTML-страницей, и разбор выдал бы бессмысленный `decode`.
    #[test]
    fn base_url_trailing_slash_is_normalized() {
        let c = VkClient::new(
            &VkConfig {
                base_url: "https://api.vk.com/method///".into(),
                ..cfg()
            },
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap();
        assert!(format!("{c:?}").contains("https://api.vk.com/method\""), "{c:?}");
    }

    /// Джиттер обязан оставаться в границах и не паниковать на нуле.
    #[test]
    fn jitter_stays_within_bounds() {
        assert_eq!(jitter_ms(0), 0);
        for _ in 0..1000 {
            assert!(jitter_ms(250) <= 250);
        }
        // Значения различаются — иначе «джиттер» был бы константой.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(jitter_ms(250));
        }
        assert!(seen.len() > 10, "джиттер вырожден: {seen:?}");
    }

    /// Пауза повтора: сетевая — с джиттером сверху базы, флудовая — ровная.
    #[test]
    fn retry_delay_matches_the_policy() {
        let c = client();
        assert_eq!(c.retry_delay(Retry::Never, 0), Duration::ZERO);
        // Код 6: лестница 1 с → 2 с, чтобы однотипные повторы не эскалировали
        // до кода 9 (flood control).
        assert_eq!(c.retry_delay(Retry::AfterOneSecond, 0), Duration::from_secs(1));
        assert_eq!(c.retry_delay(Retry::AfterOneSecond, 1), Duration::from_secs(2));
        for _ in 0..50 {
            let d = c.retry_delay(Retry::AfterNetworkBackoff, 0);
            assert!(
                d >= Duration::from_millis(500) && d <= Duration::from_millis(750),
                "{d:?}"
            );
        }
    }

    /// Тайминги подменяются для тестов, но по умолчанию боевые.
    #[test]
    fn retry_delays_are_overridable() {
        let c = client().with_retry_delays(Duration::from_millis(5), Duration::from_millis(7));
        assert_eq!(c.retry_delay(Retry::AfterOneSecond, 0), Duration::from_millis(7));
        // Лестница повторов масштабируется вместе с подменённой базой.
        assert_eq!(c.retry_delay(Retry::AfterOneSecond, 1), Duration::from_millis(14));
        assert!(c.retry_delay(Retry::AfterNetworkBackoff, 0) >= Duration::from_millis(5));
    }

    /// Клиент и состояние обязаны смотреть в ОДНУ ячейку здоровья.
    #[tokio::test]
    async fn health_handle_is_shared_with_the_state() {
        let health = Arc::new(RwLock::new(ConnHealth::default()));
        let c = VkClient::new(&cfg(), health.clone()).unwrap();

        c.note_down("boom").await;
        assert!(
            matches!(&*health.read().await, ConnHealth::Down { reason, .. } if reason == "boom")
        );

        // Повторный отказ не сдвигает `since` — иначе окно 503 не кончалось бы.
        let since_before = match &*health.read().await {
            ConnHealth::Down { since, .. } => *since,
            other => panic!("{other:?}"),
        };
        c.note_down("another").await;
        match &*health.read().await {
            ConnHealth::Down { since, reason } => {
                assert_eq!(*since, since_before);
                assert_eq!(reason, "boom", "первая причина важнее последней");
            }
            other => panic!("{other:?}"),
        }

        c.note_ok().await;
        assert!(matches!(&*health.read().await, ConnHealth::Ok { .. }));
    }

    /// Троттлер создаётся из конфига и живёт в единственном экземпляре.
    #[tokio::test]
    async fn throttle_is_built_from_the_config() {
        let c = VkClient::new(
            &VkConfig {
                rate_limit_rps: 3,
                ..cfg()
            },
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap();
        assert_eq!(c.throttle_stats().await, ThrottleStats::default());
        assert!(
            Arc::ptr_eq(&c.throttle(), &c.throttle()),
            "троттлер обязан быть один на клиент"
        );
    }

    /// Транспортная ошибка не тащит URL в текст: в query LongPoll лежит `key`.
    #[tokio::test]
    async fn transport_errors_do_not_quote_the_url() {
        let c = VkClient::new(
            &VkConfig {
                // Порт 1 закрыт всегда — connect упадёт мгновенно.
                base_url: "http://127.0.0.1:1/method".into(),
                ..cfg()
            },
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap()
        .with_retry_delays(Duration::from_millis(1), Duration::from_millis(1));

        let err = c
            .call_value("users.get", vec![("user_ids", "1".into())])
            .await
            .unwrap_err();
        assert!(err.is_transport(), "{err:?}");
        let text = err.to_string();
        assert!(!text.contains("127.0.0.1"), "URL в тексте ошибки: {text}");
        assert!(!text.contains("UNIT_TEST_TOKEN"), "{text}");
    }
}
