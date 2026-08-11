//! HTTP-клиент Bitrix24: единственная точка выхода плагина в сеть.
//!
//! ```text
//! methods.rs ─┐
//! batch.rs   ─┴→ B24Client::call_value → throttle(2 лимита) → POST JSON → parse_envelope
//!                                              ↑                              ↓
//!                                     пауза метода / ведро            ретрай + ConnHealth
//! ```
//!
//! Модуль отвечает за пять вещей и ни за что больше:
//!
//! 1. **Сборка URL.** `<portal>/rest/<uid>/<code>/<method>.json`. Секрет —
//!    ВЕСЬ путь целиком, поэтому base URL не печатается ни в `Debug`, ни в
//!    логах, ни в текстах ошибок.
//! 2. **Формат вызова.** `POST` с `Content-Type: application/json`. Вложенные
//!    структуры (`ATTACH`, `KEYBOARD`, `fields`, `filter`, `select`) в
//!    query-string ломаются, поэтому единственная поддерживаемая форма —
//!    JSON-тело.
//! 3. **Соблюдение обоих лимитов.** Один [`Throttle`] на клиент, то есть на
//!    секрет (см. [`throttle`]).
//! 4. **Ретраи.** Не более ОДНОГО повтора и только по таблице
//!    [`error::retry_for`]; методы отправки не повторяются никогда (Р9).
//! 5. **Здоровье соединения.** Успех ⇒ [`ConnHealth::Ok`], транспортный отказ
//!    ⇒ [`ConnHealth::Down`], иначе ответ `503 not_connected` был бы ни на чём
//!    не основан.
//!
//! # Почему таймаут ОБЯЗАТЕЛЕН, а не желателен
//!
//! Превышение лимита интенсивности у Bitrix24 выглядит не как JSON-ошибка, а
//! как **зависание или обрыв TCP**: портал перестаёт отвечать. Без клиентского
//! таймаута такой вызов держал бы задачу неопределённо долго и не породил бы
//! ошибки, которую можно было бы повторить. Оба таймаута (connect и общий)
//! приходят из конфига — [`crate::config::BitrixConfig::connect_timeout_secs`]
//! и [`crate::config::BitrixConfig::request_timeout_secs`].
//!
//! # Р9: отправка не повторяется никогда
//!
//! У `im.message.add` и `im.v2.File.upload` нет дедуп-идентификатора (аналога
//! `random_id` у VK): портал принимает каждый вызов как новое сообщение.
//! Повтор после обрыва — это дубль у собеседника, поэтому запрет живёт ЗДЕСЬ,
//! в транспорте, а не в вызывающем коде: любой будущий роут получает его
//! автоматически. Запрет распространяется и на `batch`, внутрь которого
//! вложена отправка.

pub mod batch;
pub mod error;
pub mod methods;
pub mod schema;
pub mod throttle;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::RwLock;

pub use batch::{BatchCall, BatchError, BatchOutcome, MAX_BATCH_COMMANDS};
pub use error::{
    classify, parse_envelope, retry_for, to_api_error, B24Error, B24Response, Retry, TimeInfo,
};
pub use throttle::{Bucket, Clock, MethodBudget, SystemClock, Throttle, ThrottleStats};

use crate::config::{BitrixConfig, WebhookUrl};
use crate::state::ConnHealth;

/// Параметры метода — JSON-объект.
///
/// Именно `Value`, а не `Vec<(&str, String)>` как у VK: у Bitrix24 половина
/// полезных параметров вложенная (`ATTACH`, `KEYBOARD`, `fields`, `filter`,
/// `select`), и плоский список пар не смог бы их выразить.
pub type Params = Value;

/// `User-Agent` плагина.
///
/// Честный, а не маскирующийся под браузер: подделка UA не даёт доступа ни к
/// чему, зато делает трафик неотличимым от чужого при разборе инцидентов.
pub const USER_AGENT: &str = concat!("devforge-bitrix/", env!("CARGO_PKG_VERSION"));

/// База паузы перед единственным повтором.
pub const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Верхняя граница джиттера, добавляемого к паузе повтора.
///
/// Нужен, чтобы N вызовов, упавших от одного обрыва, не пошли на повтор в одну
/// и ту же миллисекунду и не устроили залп по едва поднявшемуся порталу.
pub const RETRY_BACKOFF_JITTER_MS: u64 = 250;

/// Методы, которые НЕ ПОВТОРЯЮТСЯ НИКОГДА (Р9), в нижнем регистре.
///
/// Дедуп-идентификатора у них нет: повтор создаёт дубль сообщения, файла или
/// исполнения бот-команды (второй голос, повторная заявка). Список намеренно
/// короткий и явный — «угадывать» по префиксу нельзя, `im.message.update` и
/// `im.message.delete` идемпотентны и повторяться должны.
pub const NO_RETRY_METHODS: [&str; 3] =
    ["im.message.add", "im.v2.file.upload", "im.message.command"];

/// Клиент REST Bitrix24.
///
/// Создаётся ОДИН на секрет ([`crate::state::B24State::client`]): вместе с
/// клиентом создаётся троттлер, а два троттлера означали бы двойной бюджет
/// запросов при одном общем лимите портала.
pub struct B24Client {
    http: reqwest::Client,
    /// `<portal>/rest/<uid>/<code>` — СОДЕРЖИТ СЕКРЕТ ЦЕЛИКОМ.
    ///
    /// Наружу не отдаётся ничем: ни геттером, ни `Debug`, ни логом.
    base_url: String,
    throttle: Arc<Throttle>,
    health: Arc<RwLock<ConnHealth>>,
    retry_backoff: Duration,
}

impl std::fmt::Debug for B24Client {
    /// Печатает всё, кроме секрета — а секрет здесь это base URL целиком,
    /// включая хост портала.
    ///
    /// У VK в аналогичном месте адрес печатается (`api.vk.com` не секрет). У
    /// Bitrix24 так нельзя: хост однозначно указывает на портал, а путь несёт
    /// учётные данные.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("B24Client")
            .field("base_url", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl B24Client {
    /// Собирает клиент на конкретном вебхуке.
    ///
    /// База берётся из [`BitrixConfig::base_url_override`], если он задан
    /// (стенд `tests/b24_stub.rs`, правило Р17), иначе — из самого вебхука.
    ///
    /// `health` приходит снаружи (обычно
    /// [`crate::state::B24State::health_handle`]): клиент и состояние обязаны
    /// смотреть на ОДНУ ячейку здоровья, иначе data-роуты отдавали бы `503` по
    /// собственной копии, ничего не знающей о реальных вызовах.
    ///
    /// # Errors
    ///
    /// - вебхука нет и `base_url_override` пуст — звать нечего;
    /// - вебхук не разбирается регекспом §4 (`user_id` криптографически связан
    ///   с кодом, чинить строку по частям нельзя);
    /// - `reqwest::Client` не собрался (нерабочий TLS-бэкенд, исчерпание
    ///   дескрипторов).
    pub fn new(
        cfg: &BitrixConfig,
        webhook: &WebhookUrl,
        health: Arc<RwLock<ConnHealth>>,
    ) -> anyhow::Result<Self> {
        let base_url = if cfg.base_url_override.is_empty() {
            webhook.api_base().ok_or_else(|| {
                // В тексте ошибки НЕТ самой строки: она либо пуста, либо
                // содержит секрет в нераспознанной форме.
                anyhow::anyhow!("Bitrix24 webhook is missing or malformed")
            })?
        } else {
            cfg.base_url_override.trim_end_matches('/').to_string()
        };

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.request_timeout_secs))
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
            .user_agent(USER_AGENT)
            .build()?;

        Ok(Self {
            http,
            base_url,
            throttle: Arc::new(Throttle::new(
                f64::from(cfg.rate_limit_rps),
                f64::from(cfg.burst),
            )),
            health,
            retry_backoff: RETRY_BACKOFF,
        })
    }

    /// Задаёт паузу перед повтором.
    ///
    /// Данные, а не константа: иначе тест «транспортная ошибка повторяется
    /// ровно один раз» обязан был бы спать настоящие 500 мс с джиттером.
    /// Продовый путь всегда идёт через [`B24Client::new`].
    #[must_use]
    pub fn with_retry_backoff(mut self, backoff: Duration) -> Self {
        self.retry_backoff = backoff;
        self
    }

    /// Подменяет троттлер (тесты: ручные часы, свой потолок ожидания паузы).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.throttle = throttle;
        self
    }

    /// Общий троттлер клиента.
    ///
    /// Отдаётся наружу, чтобы воркер событий (Ф7) и роуты видели ОДИН бюджет и
    /// могли показать паузу метода в статусе панели (Р16).
    pub fn throttle(&self) -> Arc<Throttle> {
        self.throttle.clone()
    }

    /// Счётчики троттлера — для `GET /api/bitrix/status` и логов.
    pub async fn throttle_stats(&self) -> ThrottleStats {
        self.throttle.stats().await
    }

    // -----------------------------------------------------------------
    // Вызов метода.
    // -----------------------------------------------------------------

    /// Вызов метода с разбором `result` в типизированную структуру.
    ///
    /// # Errors
    ///
    /// Всё, что может вернуть [`B24Client::call_value`], плюс
    /// [`B24Error::Decode`] при несоответствии ответа ожидаемой форме.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Params,
    ) -> Result<T, B24Error> {
        let resp = self.call_value(method, params).await?;
        serde_json::from_value(resp.result).map_err(|e| B24Error::Decode {
            reason: format!("{method}: {e}"),
        })
    }

    /// Вызов метода с сырым конвертом в результате.
    ///
    /// Последовательность: оба лимита → `POST` JSON → [`parse_envelope`] →
    /// учёт `time` → политика ретраев.
    ///
    /// **Повтор использует ТЕ ЖЕ параметры** — но только для методов, которым
    /// повтор вообще разрешён (см. [`NO_RETRY_METHODS`] и Р9).
    ///
    /// # Errors
    ///
    /// [`B24Error::Transport`] / [`B24Error::Http`] — до портала не доехали;
    /// [`B24Error::Decode`] — ответ не разобрался; [`B24Error::Api`] — портал
    /// отказал по существу; [`B24Error::Throttled`] /
    /// [`B24Error::OperatingLimit`] — упёрлись в один из двух лимитов.
    pub async fn call_value(&self, method: &str, params: Params) -> Result<B24Response, B24Error> {
        let retryable_method = is_retryable(method, &params);
        let mut attempt: u32 = 0;
        loop {
            // Лимит 2: метод может стоять на паузе. Длинную паузу не
            // пересиживаем — заведомо отказной запрос всё равно был бы
            // засчитан порталом в тот же лимит.
            if let Err(left) = self.throttle.acquire(method).await {
                let reset_at = chrono::Utc::now().timestamp() + left.as_secs() as i64;
                tracing::warn!(
                    target: "forge_bitrix",
                    method,
                    pause_secs = left.as_secs(),
                    "forge-bitrix: skipping the call, the method is paused by the operating-time limit"
                );
                return Err(B24Error::OperatingLimit {
                    reset_at: Some(reset_at),
                });
            }

            let started = Instant::now();
            let outcome = self.post_once(method, &params).await;
            let elapsed_ms = started.elapsed().as_millis() as u64;

            let err = match outcome {
                Ok(resp) => {
                    if let Some(time) = resp.time.as_ref() {
                        // ЕДИНСТВЕННЫЙ источник данных о втором лимите.
                        self.throttle.note_success(method, time).await;
                    }
                    self.note_ok().await;
                    tracing::debug!(
                        target: "forge_bitrix",
                        method,
                        elapsed_ms,
                        attempt,
                        operating = resp.time.map(|t| t.operating).unwrap_or_default(),
                        "forge-bitrix: portal call succeeded"
                    );
                    return Ok(resp);
                }
                Err(e) => e,
            };

            // Второй лимит: пауза ставится ДО принятия решения о повторе —
            // повторять всё равно нельзя, но соседние вызовы этого же метода
            // не должны идти в сеть за отказом.
            if let B24Error::OperatingLimit { reset_at } = &err {
                let until = self.throttle.note_operating_limit(method, *reset_at).await;
                return Err(B24Error::OperatingLimit {
                    reset_at: Some(until),
                });
            }

            if err.is_transport() {
                self.note_down(err.to_string()).await;
            }

            let retry = retry_for(&err);
            if attempt == 0 && retry == Retry::AfterBackoff && retryable_method {
                let delay = self.retry_delay();
                // В лог уходят метод, длительность и КОД ошибки — но НИКОГДА
                // ни URL, ни параметры: и то, и другое цитирует секрет.
                tracing::warn!(
                    target: "forge_bitrix",
                    method,
                    elapsed_ms,
                    error_code = err.code().unwrap_or("transport"),
                    retry_in_ms = delay.as_millis() as u64,
                    "forge-bitrix: portal call failed, retrying once"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }

            if !retryable_method && retry == Retry::AfterBackoff {
                // Р9 своими словами — чтобы при разборе инцидента не искали,
                // почему повтора не было.
                tracing::warn!(
                    target: "forge_bitrix",
                    method,
                    "forge-bitrix: a send method failed and is NOT retried — a repeat would duplicate the message"
                );
            }
            tracing::warn!(
                target: "forge_bitrix",
                method,
                elapsed_ms,
                attempt,
                error_code = err.code().unwrap_or("transport"),
                "forge-bitrix: portal call failed"
            );
            return Err(err);
        }
    }

    /// Одна попытка: собрать URL, отправить JSON-тело, разобрать конверт.
    ///
    /// ЕДИНСТВЕННОЕ место, где секрет (`base_url`) попадает в запрос.
    async fn post_once(&self, method: &str, params: &Params) -> Result<B24Response, B24Error> {
        let url = format!("{}/{}.json", self.base_url, method);
        let resp = self
            .http
            .post(&url)
            .json(params)
            .send()
            .await
            .map_err(error::transport_error)?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(error::transport_error)?;
        parse_envelope(status, &body)
    }

    // -----------------------------------------------------------------
    // Скачивание байтов (Ф8).
    // -----------------------------------------------------------------

    /// `GET` по готовому адресу — ПОТОКОМ, без чтения тела в память.
    ///
    /// Единственные два вызывающих: скачивание вложения по `downloadUrl`
    /// (адрес выдал `im.v2.File.download` и он СОДЕРЖИТ КОД ВЕБХУКА) и
    /// скачивание аватара с публичного CDN портала. Оба живут в
    /// [`crate::routes::media`].
    ///
    /// # Чем отличается от [`B24Client::call_value`]
    ///
    /// 1. **Троттлер не трогается.** Оба лимита портала считаются по вызовам
    ///    REST-МЕТОДОВ, а раздача файла к ним не относится: сам
    ///    `im.v2.File.download` бюджет уже занял. Пропускать байты через ведро
    ///    2 rps значило бы растянуть открытие галереи на минуты.
    /// 2. **Повтора нет.** Ссылка одноразовая и короткоживущая: повторять по
    ///    ней бессмысленно, а осмысленный повтор — это заново добытый адрес,
    ///    и решает его вызывающий.
    /// 3. **Тело не читается.** Возвращается `Response`; читать его потоком —
    ///    работа вызывающего, иначе стомегабайтный файл лёг бы в память
    ///    целиком.
    ///
    /// В логи уходит только статус и факт отказа: URL не логируется ни на
    /// каком уровне, включая `trace`.
    ///
    /// # Errors
    ///
    /// [`B24Error::Transport`] — до сервера не доехали (текст очищен от URL
    /// через [`error::transport_error`]); [`B24Error::Http`] — сервер ответил
    /// не-2xx.
    pub async fn get_stream(&self, url: &str) -> Result<reqwest::Response, B24Error> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(error::transport_error)?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                target: "forge_bitrix",
                status = status.as_u16(),
                "forge-bitrix: byte stream refused"
            );
            if status.is_server_error() {
                self.note_down(format!("byte stream refused with HTTP {status}"))
                    .await;
            }
            return Err(B24Error::Http {
                status: status.as_u16(),
            });
        }
        Ok(resp)
    }

    /// Пауза перед единственным повтором (база + джиттер).
    fn retry_delay(&self) -> Duration {
        self.retry_backoff + Duration::from_millis(jitter_ms(RETRY_BACKOFF_JITTER_MS))
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
    /// окно `503` бесконечно, и здоровье уже никогда не починилось бы (правило
    /// дословно повторяет [`crate::state::B24State::note_down`]).
    ///
    /// Причина приходит из [`B24Error`], то есть уже очищена от URL, — но
    /// очистка повторяется здесь же: причина уезжает в `/status`, и полагаться
    /// на дисциплину вызывающего в этом месте нельзя.
    async fn note_down(&self, reason: impl Into<String>) {
        let mut guard = self.health.write().await;
        if matches!(&*guard, ConnHealth::Down { .. }) {
            return;
        }
        *guard = ConnHealth::Down {
            since: chrono::Utc::now().timestamp(),
            reason: error::scrub_urls(&reason.into()),
        };
    }
}

/// Разрешён ли повтор этого вызова (Р9).
///
/// Проверяются ДВЕ формы:
///
/// 1. прямой вызов метода отправки;
/// 2. `batch`, внутрь которого вложена отправка — иначе запрет обходился бы
///    одной строкой `{"cmd":{"send":"im.message.add?..."}}`, и дубль сообщения
///    вернулся бы тем же путём, каким его закрыли.
///
/// Сравнение регистронезависимое: портал принимает имена методов в любом
/// регистре, а значит, и обходить запрет регистром нельзя.
fn is_retryable(method: &str, params: &Params) -> bool {
    let lower = method.to_ascii_lowercase();
    if NO_RETRY_METHODS.contains(&lower.as_str()) {
        return false;
    }
    if lower == "batch" {
        let cmds = params
            .get("cmd")
            .map(|v| v.to_string().to_ascii_lowercase())
            .unwrap_or_default();
        if NO_RETRY_METHODS.iter().any(|m| cmds.contains(m)) {
            return false;
        }
    }
    true
}

/// Псевдослучайные миллисекунды `0..=max` без крейта `rand`.
///
/// Источник — UUID v4 (крейт уже в дереве ради `upload_id`). Криптографического
/// качества здесь не нужно: джиттер лишь разводит одновременные повторы.
fn jitter_ms(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    (uuid::Uuid::new_v4().as_u128() as u64) % (max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BitrixConfig {
        BitrixConfig {
            cache_dir: std::env::temp_dir().join("forge-bitrix-tests"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: std::env::temp_dir().join("forge-bitrix-tests").join("prompts.json"),
            claude_cli_path: std::env::temp_dir().join("forge-bitrix-tests").join("no-such-claude"),
            ..BitrixConfig::default()
        }
    }

    fn webhook() -> WebhookUrl {
        WebhookUrl::new("https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/")
    }

    fn client() -> B24Client {
        B24Client::new(
            &cfg(),
            &webhook(),
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap()
    }

    /// `Debug` клиента не должен раскрывать ни кода вебхука, ни хоста портала:
    /// у Bitrix24 секрет — это весь URL.
    #[test]
    fn debug_never_leaks_the_webhook() {
        let dbg = format!("{:?}", client());
        assert!(!dbg.contains("wqdzbfhmn83d23j4"), "{dbg}");
        assert!(!dbg.contains("test-work-2.bitrix24.ru"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
    }

    #[test]
    fn the_base_url_comes_from_the_webhook_and_the_override_wins() {
        // Без вебхука и без override клиент не собирается: звать нечего.
        let err = B24Client::new(
            &cfg(),
            &WebhookUrl::default(),
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap_err();
        assert!(!format!("{err}").contains("rest/"), "{err}");

        // Нераспознанная строка — тоже отказ, а не «попробуем как есть».
        assert!(B24Client::new(
            &cfg(),
            &WebhookUrl::new("не-урл"),
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .is_err());

        // Override побеждает и работает БЕЗ вебхука — так живёт стенд.
        let c = B24Client::new(
            &BitrixConfig {
                base_url_override: "http://127.0.0.1:9/rest/1/stub/".into(),
                ..cfg()
            },
            &WebhookUrl::default(),
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap();
        assert_eq!(c.base_url, "http://127.0.0.1:9/rest/1/stub");
    }

    /// Р9 своими словами: список методов без повтора — контракт, а не деталь.
    #[test]
    fn send_methods_are_never_retryable_in_any_form() {
        let empty = serde_json::json!({});
        assert!(!is_retryable("im.message.add", &empty));
        assert!(!is_retryable("IM.MESSAGE.ADD", &empty));
        assert!(!is_retryable("im.v2.File.upload", &empty));

        // Идемпотентные соседи повторяться обязаны.
        assert!(is_retryable("im.message.update", &empty));
        assert!(is_retryable("im.message.delete", &empty));
        assert!(is_retryable("im.recent.get", &empty));

        // …и запрет не обходится вложением в batch.
        let smuggled = serde_json::json!({
            "halt": 0,
            "cmd": {"a": "im.counters.get", "b": "im.message.add?DIALOG_ID=chat7&MESSAGE=hi"}
        });
        assert!(!is_retryable("batch", &smuggled));
        let harmless = serde_json::json!({"halt": 0, "cmd": {"a": "im.recent.get"}});
        assert!(is_retryable("batch", &harmless));
    }

    /// Джиттер обязан оставаться в границах и не паниковать на нуле.
    #[test]
    fn jitter_stays_within_bounds() {
        assert_eq!(jitter_ms(0), 0);
        for _ in 0..1000 {
            assert!(jitter_ms(250) <= 250);
        }
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(jitter_ms(250));
        }
        assert!(seen.len() > 10, "джиттер вырожден: {seen:?}");
    }

    #[test]
    fn retry_delay_is_the_backoff_plus_jitter() {
        let c = client();
        for _ in 0..50 {
            let d = c.retry_delay();
            assert!(
                d >= RETRY_BACKOFF && d <= RETRY_BACKOFF + Duration::from_millis(250),
                "{d:?}"
            );
        }
        let c = client().with_retry_backoff(Duration::from_millis(1));
        assert!(c.retry_delay() <= Duration::from_millis(251));
    }

    /// Клиент и состояние обязаны смотреть в ОДНУ ячейку здоровья.
    #[tokio::test]
    async fn health_handle_is_shared_with_the_state() {
        let health = Arc::new(RwLock::new(ConnHealth::default()));
        let c = B24Client::new(&cfg(), &webhook(), health.clone()).unwrap();

        c.note_down("boom").await;
        assert!(
            matches!(&*health.read().await, ConnHealth::Down { reason, .. } if reason == "boom")
        );

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

    /// Причина отказа тоже проходит очистку: она уезжает в `/status`.
    #[tokio::test]
    async fn the_health_reason_never_quotes_the_url() {
        let health = Arc::new(RwLock::new(ConnHealth::default()));
        let c = B24Client::new(&cfg(), &webhook(), health.clone()).unwrap();
        c.note_down("failed at https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/profile.json")
            .await;
        match &*health.read().await {
            ConnHealth::Down { reason, .. } => {
                assert!(!reason.contains("wqdzbfhmn83d23j4"), "{reason}");
                assert!(!reason.contains("bitrix24.ru"), "{reason}");
            }
            other => panic!("{other:?}"),
        };
    }

    /// Троттлер создаётся из конфига и живёт в единственном экземпляре.
    #[tokio::test]
    async fn one_throttle_per_client() {
        let c = client();
        assert_eq!(c.throttle_stats().await, ThrottleStats::default());
        assert!(Arc::ptr_eq(&c.throttle(), &c.throttle()));
    }

    /// Транспортная ошибка не тащит URL в текст — а в URL Bitrix24 лежит весь
    /// секрет. Порт 1 закрыт всегда, сети тест не требует.
    #[tokio::test]
    async fn transport_errors_never_quote_the_url() {
        let c = B24Client::new(
            &BitrixConfig {
                base_url_override: "http://127.0.0.1:1/rest/1/wqdzbfhmn83d23j4".into(),
                connect_timeout_secs: 1,
                request_timeout_secs: 1,
                ..cfg()
            },
            &WebhookUrl::default(),
            Arc::new(RwLock::new(ConnHealth::default())),
        )
        .unwrap()
        .with_retry_backoff(Duration::from_millis(1));

        let err = c
            .call_value("profile", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.is_transport(), "{err:?}");
        let text = err.to_string();
        assert!(!text.contains("127.0.0.1"), "URL в тексте ошибки: {text}");
        assert!(!text.contains("wqdzbfhmn83d23j4"), "{text}");
    }
}
