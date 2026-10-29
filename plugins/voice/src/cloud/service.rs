//! Служебные вызовы polza.ai: `GET /key`, `GET /balance`, `GET /models`.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Живость ключа проверяется через `/key`, а не пробным запросом
//!
//! Вызов бесплатный и мгновенный, и он же показывает остаток лимита. Пробный
//! `chat/completions` стоил бы денег на каждом открытии настроек, а
//! `/audio/transcriptions` — ещё и минуты стены.
//!
//! `/credits`, `/me`, `/auth/key` отвечают 404 — их нет.
//!
//! # ГЛАВНАЯ ЛОВУШКА КАТАЛОГА: цена лежит на уровень глубже
//!
//! Цена модели живёт в `top_provider.pricing`. Поля `pricing` верхнего уровня
//! у записей НЕТ ВООБЩЕ (проверено на всех 388 записях живого каталога), и
//! парсер, который читает верхний уровень, не падает и не ругается — он молча
//! показывает «бесплатно». Ошибка такого рода обнаруживается счётом в конце
//! месяца, поэтому на неё есть отдельный тест
//! ([`tests::the_price_is_read_from_top_provider_and_not_from_the_top_level`]).
//!
//! Формы цены две:
//!
//! ```json
//! {"prompt_per_million": "176.05", "completion_per_million": "1056.32", "currency": "RUB"}
//! {"stt_per_minute": "0.43200000", "currency": "RUB"}
//! ```
//!
//! Значения — СТРОКИ. Разбор идёт через [`super::number`], который принимает и
//! строку, и число: лимиты ключа рядом приходят числами.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::{number, text, CloudClient, CloudError};

/// Тип модели в каталоге: `chat`, `stt`, `tts`, `image`, `video`,
/// `embedding`, `music`.
pub const KIND_CHAT: &str = "chat";
pub const KIND_STT: &str = "stt";

/// Лимиты ключа — ответ `GET /key`.
///
/// Поля `Option`, потому что это ЧУЖОЙ ответ: пропавшее поле обязано означать
/// «не знаю», а не ноль. Ноль в остатке лимита выглядел бы как исчерпанный
/// ключ, и человек чинил бы не то.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct KeyInfo {
    /// Маска ключа, которую polza.ai отдаёт сам (`pza_RIcg••••…Uzu4js`).
    pub label: Option<String>,
    pub limit: Option<f64>,
    pub limit_remaining: Option<f64>,
    /// Период сброса лимита. **Живой ответ — `monthly`**, а не `daily`, как
    /// было записано в плане; поэтому строка, а не перечисление: чужое
    /// значение не имеет права ломать разбор.
    pub limit_reset: Option<String>,
    pub usage: Option<f64>,
}

impl KeyInfo {
    /// Исчерпан ли лимит. `None` — сервис про лимит ничего не сказал.
    #[must_use]
    pub fn exhausted(&self) -> Option<bool> {
        self.limit_remaining.map(|r| r <= 0.0)
    }
}

/// Баланс — ответ `GET /balance`. **В РУБЛЯХ.**
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Balance {
    pub amount: f64,
    pub reserved: f64,
    pub spent: f64,
    /// Валюта не приходит в ответе: она зафиксирована пробой и подписана здесь,
    /// чтобы «461» на экране не читалось как доллары.
    pub currency: &'static str,
}

/// Цена модели. Одна структура на обе формы: у чата — за миллион токенов, у
/// расшифровки — за минуту.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Pricing {
    pub prompt_per_million: Option<f64>,
    pub completion_per_million: Option<f64>,
    pub stt_per_minute: Option<f64>,
    pub currency: Option<String>,
}

impl Pricing {
    /// Есть ли хоть одна цена. Пустая структура означает «цену не отдали», и
    /// это НЕ то же самое, что «бесплатно».
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prompt_per_million.is_none()
            && self.completion_per_million.is_none()
            && self.stt_per_minute.is_none()
    }
}

/// Запись каталога моделей.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    /// `chat` / `stt` / `tts` / `image` / `video` / `embedding` / `music`.
    pub kind: String,
    pub context_length: Option<u64>,
    pub max_completion_tokens: Option<u64>,
    /// Цена ИЗ `top_provider.pricing`. `None` — цены в ответе не было.
    pub pricing: Option<Pricing>,
}

impl ModelInfo {
    #[must_use]
    pub fn is_kind(&self, kind: &str) -> bool {
        self.kind == kind
    }
}

/// Модели одного типа. Отдельно `chat` для саммари, отдельно `stt` для
/// расшифровки: смешанный список во фронте предлагал бы выбрать генератор
/// картинок в качестве саммаризатора.
#[must_use]
pub fn filter_kind(models: &[ModelInfo], kind: &str) -> Vec<ModelInfo> {
    models.iter().filter(|m| m.is_kind(kind)).cloned().collect()
}

// --- разбор (чистые функции) --------------------------------------------------

/// Разбирает ответ `GET /key`.
#[must_use]
pub fn parse_key(v: &Value) -> KeyInfo {
    KeyInfo {
        label: text(v.get("label")),
        limit: number(v.get("limit")),
        limit_remaining: number(v.get("limit_remaining")),
        limit_reset: text(v.get("limit_reset")),
        usage: number(v.get("usage")),
    }
}

/// Разбирает ответ `GET /balance`. Суммы приходят СТРОКАМИ.
#[must_use]
pub fn parse_balance(v: &Value) -> Balance {
    Balance {
        amount: number(v.get("amount")).unwrap_or_default(),
        reserved: number(v.get("reservedAmount")).unwrap_or_default(),
        spent: number(v.get("spentAmount")).unwrap_or_default(),
        currency: "RUB",
    }
}

/// Разбирает цену. **Читает ТОЛЬКО `top_provider.pricing`.**
///
/// См. разбор ловушки в шапке модуля: верхнего уровня у цены не существует.
#[must_use]
pub fn parse_pricing(model: &Value) -> Option<Pricing> {
    let node = model.get("top_provider")?.get("pricing")?;
    if !node.is_object() {
        return None;
    }
    let p = Pricing {
        prompt_per_million: number(node.get("prompt_per_million")),
        completion_per_million: number(node.get("completion_per_million")),
        stt_per_minute: number(node.get("stt_per_minute")),
        currency: text(node.get("currency")),
    };
    (!p.is_empty()).then_some(p)
}

/// Разбирает ответ `GET /models`: конверт `{"object":"list","data":[…]}`.
pub fn parse_models(v: &Value) -> Result<Vec<ModelInfo>, CloudError> {
    let items = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| CloudError::Decode("в каталоге моделей нет массива data".into()))?;
    Ok(items
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?.to_string();
            let tp = m.get("top_provider");
            Some(ModelInfo {
                id,
                name: text(m.get("name")),
                kind: text(m.get("type")).unwrap_or_default(),
                // Ноль в `context_length` у STT-моделей означает «неприменимо»,
                // и показывать его как настоящий контекст нельзя.
                context_length: tp
                    .and_then(|t| number(t.get("context_length")))
                    .map(|n| n as u64)
                    .filter(|n| *n > 0),
                max_completion_tokens: tp
                    .and_then(|t| number(t.get("max_completion_tokens")))
                    .map(|n| n as u64),
                pricing: parse_pricing(m),
            })
        })
        .collect())
}

// --- вызовы -------------------------------------------------------------------

impl CloudClient {
    /// `GET /key` — лимиты ключа. **Бесплатно.**
    pub async fn key_info(&self, timeout: Duration) -> Result<KeyInfo, CloudError> {
        let v = self.send_json(self.get("key"), timeout).await?;
        Ok(parse_key(&v))
    }

    /// `GET /balance` — остаток в рублях.
    pub async fn balance(&self, timeout: Duration) -> Result<Balance, CloudError> {
        let v = self.send_json(self.get("balance"), timeout).await?;
        Ok(parse_balance(&v))
    }

    /// `GET /models` — каталог целиком (около 400 записей).
    pub async fn models(&self, timeout: Duration) -> Result<Vec<ModelInfo>, CloudError> {
        let v = self.send_json(self.get("models"), timeout).await?;
        parse_models(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Ответ `GET /key`, снятый с живого API 2026-08-19.
    fn live_key() -> Value {
        json!({
            "creator_user_id": "usr_2109015473186291713",
            "label": "pza_RIcg••••••••••••••••••••Uzu4js",
            "limit": 300,
            "limit_remaining": 288.7275241,
            "limit_reset": "monthly",
            "usage": 11.2724759,
            "usage_daily": 11.2724759,
            "usage_weekly": 11.2724759,
            "usage_monthly": 11.2724759
        })
    }

    /// Ответ `GET /balance`, снятый с живого API. Суммы — СТРОКИ.
    fn live_balance() -> Value {
        json!({
            "amount": "461.88706171",
            "reservedAmount": "0.00000000",
            "spentAmount": "138.72166217",
            "updatedAt": "2026-08-18T22:27:57.610Z"
        })
    }

    /// Каталог: чат и STT в РЕАЛЬНОЙ форме — с `pricing` на верхнем уровне,
    /// выставленным в `null`, и настоящей ценой уровнем глубже.
    pub(crate) fn live_models() -> Value {
        json!({"object": "list", "data": [
            {
                "id": "openai/whisper-1", "object": "model", "name": "Whisper 1", "type": "stt",
                "pricing": null,
                "top_provider": {
                    "name": "openai", "context_length": 0, "max_completion_tokens": null,
                    "pricing": {"stt_per_minute": "0.43200000", "currency": "RUB"}
                },
                "endpoints": ["/api/v1/audio/transcriptions", "/api/v1/media"]
            },
            {
                "id": "google/gemini-3.5-flash", "object": "model",
                "name": "Google: Gemini 3.5 Flash", "type": "chat",
                "pricing": null,
                "top_provider": {
                    "name": "google-vertex/global", "context_length": 1048576,
                    "max_completion_tokens": 65536,
                    "pricing": {"prompt_per_million": "176.05000000",
                                "completion_per_million": "1056.32000000",
                                "currency": "RUB"}
                }
            },
            {
                "id": "openai/whisper-large-v3-turbo", "object": "model", "type": "stt",
                "pricing": null,
                "top_provider": {"context_length": 0,
                    "pricing": {"stt_per_minute": "0.04800000", "currency": "RUB"}}
            },
            {"id": "some/image-model", "object": "model", "type": "image", "pricing": null,
             "top_provider": {"pricing": {"currency": "RUB"}}}
        ]})
    }

    #[test]
    fn the_key_limits_are_parsed_from_the_live_shape() {
        let k = parse_key(&live_key());
        assert_eq!(k.limit, Some(300.0));
        assert_eq!(k.limit_remaining, Some(288.7275241));
        // ЖИВОЙ ответ говорит `monthly`, а план обещал `daily` — поэтому строка.
        assert_eq!(k.limit_reset.as_deref(), Some("monthly"));
        assert_eq!(k.usage, Some(11.2724759));
        assert_eq!(k.exhausted(), Some(false));
        assert!(k.label.unwrap().contains('•'), "маску отдаёт сам сервис");

        // Пустой ответ — не ноль, а «не знаю».
        let empty = parse_key(&json!({}));
        assert_eq!(empty.limit_remaining, None);
        assert_eq!(empty.exhausted(), None);
        assert_eq!(
            parse_key(&json!({"limit_remaining": 0})).exhausted(),
            Some(true)
        );
    }

    #[test]
    fn the_balance_is_parsed_from_strings_and_is_in_roubles() {
        let b = parse_balance(&live_balance());
        assert!((b.amount - 461.88706171).abs() < 1e-8, "{b:?}");
        assert_eq!(b.reserved, 0.0);
        assert!((b.spent - 138.72166217).abs() < 1e-8, "{b:?}");
        assert_eq!(b.currency, "RUB");
    }

    /// **ГЛАВНЫЙ ТЕСТ ЗАДАЧИ.** Парсер, читающий `pricing` верхнего уровня,
    /// обязан покраснеть здесь.
    ///
    /// В фикстуре верхний уровень выставлен в `null` ровно так, как в живом
    /// каталоге, а настоящая цена лежит в `top_provider.pricing`. Наивный
    /// разбор не упал бы и не выдал ошибку — он показал бы «бесплатно», и
    /// узнали бы об этом по счёту.
    #[test]
    fn the_price_is_read_from_top_provider_and_not_from_the_top_level() {
        let models = parse_models(&live_models()).unwrap();
        let chat = models
            .iter()
            .find(|m| m.id == "google/gemini-3.5-flash")
            .unwrap();
        let p = chat.pricing.as_ref().expect(
            "цена не прочиталась: парсер смотрит на верхний уровень, где у ВСЕХ записей null",
        );
        assert_eq!(p.prompt_per_million, Some(176.05));
        assert_eq!(p.completion_per_million, Some(1056.32));
        assert_eq!(p.currency.as_deref(), Some("RUB"));
        assert_eq!(chat.context_length, Some(1_048_576));
        assert_eq!(chat.max_completion_tokens, Some(65_536));

        let stt = models.iter().find(|m| m.id == "openai/whisper-1").unwrap();
        let sp = stt.pricing.as_ref().expect("цена STT не прочиталась");
        assert_eq!(sp.stt_per_minute, Some(0.432));
        assert_eq!(sp.prompt_per_million, None, "у STT формы за токены нет");
        assert_eq!(
            stt.context_length, None,
            "context_length=0 у STT означает «неприменимо», а не нулевой контекст"
        );

        // И контрольный выстрел по самому наивному разбору: в исходной записи
        // верхний уровень действительно пуст.
        let raw = live_models();
        let first = &raw["data"][0];
        assert!(first["pricing"].is_null(), "фикстура перестала быть живой");
        assert!(!first["top_provider"]["pricing"].is_null());
    }

    /// Цена без единого числа — это «цену не отдали», а не «бесплатно».
    #[test]
    fn a_pricing_object_without_numbers_is_not_a_price() {
        let models = parse_models(&live_models()).unwrap();
        let img = models.iter().find(|m| m.id == "some/image-model").unwrap();
        assert!(img.pricing.is_none(), "{:?}", img.pricing);
        assert!(parse_pricing(&json!({"top_provider": {}})).is_none());
        assert!(parse_pricing(&json!({})).is_none());
    }

    #[test]
    fn models_are_filtered_by_type() {
        let models = parse_models(&live_models()).unwrap();
        let stt = filter_kind(&models, KIND_STT);
        let chat = filter_kind(&models, KIND_CHAT);
        assert_eq!(stt.len(), 2, "{stt:?}");
        assert!(stt.iter().all(|m| m.kind == "stt"));
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].id, "google/gemini-3.5-flash");
        assert!(filter_kind(&models, "tts").is_empty());
        // Модель расшифровки, которую выбирать НЕЛЬЗЯ, в каталоге всё равно
        // есть: фильтр по типу её не отсеивает, отсеивает выбор по умолчанию.
        assert!(stt.iter().any(|m| m.id == "openai/whisper-large-v3-turbo"));
    }

    #[test]
    fn a_broken_catalogue_is_a_named_error() {
        let e = parse_models(&json!({"object": "list"})).unwrap_err();
        assert_eq!(e.code(), "cloud_bad_response");
        // Запись без `id` пропускается, а не роняет весь каталог.
        let ok = parse_models(&json!({"data": [{"type": "chat"}, {"id": "a/b", "type": "chat"}]}))
            .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].id, "a/b");
    }
}
