//! `GET /api/meet/cloud/models` и `GET /api/meet/cloud/balance` — каталог
//! облачных моделей и остаток средств для вкладки настроек.
//!
//! # Почему это роуты, а не запрос браузера напрямую в polza.ai
//!
//! Потому что у polza.ai нет способа ответить браузеру без ключа: каталог и
//! баланс требуют `Authorization: Bearer pza_…`. Отдать ключ во фронт значит
//! положить его в память страницы, в devtools и в кэш PWA — то есть перечеркнуть
//! всё, ради чего [`crate::cloud::SecretKey`] не печатается даже в `Debug`.
//! Поэтому ходит сервер, а наружу уезжают только цифры.
//!
//! # Оба роута отвечают 200 ВСЕГДА
//!
//! Отказ облака здесь — ПОЛЕ `error`, а не статус ответа, ровно как в
//! облачном блоке `GET /api/meet/status`. Причина та же и стоит повторения:
//! вкладку настроек открывают именно тогда, когда что-то не работает, и роут,
//! падающий вместе с чужим сервисом, оставил бы человека без единственного
//! места, где чинится ключ. «Ключа нет» и «сервис молчит» — разные ветки, и
//! обе обязаны быть различимы в теле ответа.
//!
//! # Каталог не кэшируется, баланс тоже
//!
//! Оба вызова случаются по ОТКРЫТИЮ ВКЛАДКИ НАСТРОЕК, а не по загрузке
//! страницы: `status` их не зовёт (второй сетевой вызов внутри роута, который
//! гейтит панель, подвесил бы панель). Частота — единицы раз в день, и кэш тут
//! стоил бы больше, чем экономил: устаревший баланс после пополнения
//! выглядит как «деньги не пришли».

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::cloud::service::{Balance, KeyInfo, Pricing, KIND_CHAT, KIND_STT};
use crate::cloud::{CloudClient, CloudError};
use crate::state::MeetState;

use super::CloudErrorDto;

/// Потолок на каталог моделей.
///
/// Больше, чем у пробы ключа в `status`: записей около четырёхсот, и это
/// заметный ответ. Роут не гейтит панель, поэтому ждать здесь можно дольше —
/// но не бесконечно: вкладка настроек с вечным «загружаем» ничем не лучше
/// ошибки.
pub const CATALOGUE_TIMEOUT: Duration = Duration::from_secs(15);

/// Потолок на баланс и лимиты ключа.
pub const BALANCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Одна модель каталога глазами интерфейса.
#[derive(Debug, Clone, Serialize)]
pub struct CloudModelDto {
    pub id: String,
    pub name: Option<String>,
    /// `chat` | `stt`.
    pub kind: String,
    pub context_length: Option<u64>,
    pub max_completion_tokens: Option<u64>,
    /// Цена ИЗ `top_provider.pricing`. `None` — цену не отдали, и это НЕ
    /// «бесплатно»: разбор верхнего уровня, где у всех записей пусто, молча
    /// показывал бы ноль (см. шапку [`crate::cloud::service`]).
    pub pricing: Option<Pricing>,
    /// Выбрана ли она прямо сейчас в конфиге.
    pub selected: bool,
    /// **Даёт ли модель таймкоды.** Только у `stt`; у чата `None`.
    ///
    /// `Some(false)` означает «выбор этой модели ломает атрибуцию спикеров
    /// целиком», а не «немного хуже», поэтому флаг приезжает с сервера, а не
    /// вычисляется во фронте по зашитому имени.
    pub timestamps: Option<bool>,
}

/// Тело `GET /api/meet/cloud/models`.
#[derive(Debug, Clone, Serialize)]
pub struct CloudModelsDto {
    /// Есть ли ключ. `false` — списки пусты и спрашивать было нечего.
    pub configured: bool,
    /// Модели расшифровки (`type: stt`).
    pub stt: Vec<CloudModelDto>,
    /// Модели саммари (`type: chat`).
    pub chat: Vec<CloudModelDto>,
    /// Сколько записей отдал каталог ВСЕГО — включая tts, image, video,
    /// embedding и music, которые здесь не нужны.
    pub total: usize,
    /// Почему каталог не приехал. Поле, а не статус ответа — см. шапку.
    pub error: Option<CloudErrorDto>,
}

/// Тело `GET /api/meet/cloud/balance`.
#[derive(Debug, Clone, Serialize)]
pub struct CloudBalanceDto {
    pub configured: bool,
    /// Остаток В РУБЛЯХ. Валюта подписана в самой структуре, чтобы «461» не
    /// читалось как доллары.
    pub balance: Option<Balance>,
    /// Лимиты ключа из бесплатного `GET /key`. У живого ключа `limit_reset` —
    /// `monthly`, а не `daily`, поэтому поле строка.
    pub key: Option<KeyInfo>,
    pub error: Option<CloudErrorDto>,
}

/// Ошибка облака в виде поля ответа.
fn as_field(e: &CloudError) -> CloudErrorDto {
    CloudErrorDto {
        code: e.code(),
        message: e.message(),
    }
}

/// Отказ по таймауту нашего собственного потолка.
fn timed_out(secs: u64) -> CloudErrorDto {
    CloudErrorDto {
        code: "cloud_timeout",
        message: format!("polza.ai не ответил за {secs} с"),
    }
}

/// `GET /api/meet/cloud/models` — каталог моделей, разложенный по стадиям.
pub async fn models(State(state): State<Arc<MeetState>>) -> Json<CloudModelsDto> {
    let cfg = &state.config;
    let mut dto = CloudModelsDto {
        configured: cfg.cloud_configured(),
        stt: Vec::new(),
        chat: Vec::new(),
        total: 0,
        error: None,
    };
    if !dto.configured {
        return Json(dto);
    }

    let client = match CloudClient::new(&cfg.cloud()) {
        Ok(c) => c,
        Err(e) => {
            dto.error = Some(as_field(&e));
            return Json(dto);
        }
    };
    let call = client.models(CATALOGUE_TIMEOUT);
    let all = match tokio::time::timeout(CATALOGUE_TIMEOUT, call).await {
        Ok(Ok(list)) => list,
        Ok(Err(e)) => {
            dto.error = Some(as_field(&e));
            return Json(dto);
        }
        Err(_) => {
            dto.error = Some(timed_out(CATALOGUE_TIMEOUT.as_secs()));
            return Json(dto);
        }
    };

    dto.total = all.len();
    for m in all {
        let stt = m.kind == KIND_STT;
        if !stt && m.kind != KIND_CHAT {
            // tts, image, video, embedding, music — выбрать их стадией
            // невозможно, и место в списке они занимали бы зря.
            continue;
        }
        let selected = if stt {
            m.id == cfg.cloud_stt_model
        } else {
            m.id == cfg.cloud_summary_model
        };
        let row = CloudModelDto {
            timestamps: stt.then(|| crate::cloud::stt_gives_timestamps(&m.id)),
            id: m.id,
            name: m.name,
            kind: m.kind,
            context_length: m.context_length,
            max_completion_tokens: m.max_completion_tokens,
            pricing: m.pricing,
            selected,
        };
        if stt {
            dto.stt.push(row);
        } else {
            dto.chat.push(row);
        }
    }
    // Порядок каталога чужой и меняется от ответа к ответу; список, который
    // переставляется сам, невозможно читать глазами.
    dto.stt.sort_by(|a, b| a.id.cmp(&b.id));
    dto.chat.sort_by(|a, b| a.id.cmp(&b.id));
    Json(dto)
}

/// `GET /api/meet/cloud/balance` — остаток средств и лимиты ключа.
///
/// Два вызова к сервису: платный по смыслу `GET /balance` и бесплатный
/// `GET /key`. Второй берётся из кэша состояния ([`MeetState::cloud_probe`]),
/// поэтому в сеть уходит ровно один новый запрос.
pub async fn balance(State(state): State<Arc<MeetState>>) -> Json<CloudBalanceDto> {
    let cfg = &state.config;
    let mut dto = CloudBalanceDto {
        configured: cfg.cloud_configured(),
        balance: None,
        key: None,
        error: None,
    };
    if !dto.configured {
        return Json(dto);
    }
    // Лимиты — из кэшированной пробы: она уже сделана ради `status`, и второй
    // запрос за теми же числами был бы платой за ничего.
    dto.key = state.cloud_probe().await.and_then(|p| p.key);

    let client = match CloudClient::new(&cfg.cloud()) {
        Ok(c) => c,
        Err(e) => {
            dto.error = Some(as_field(&e));
            return Json(dto);
        }
    };
    let call = client.balance(BALANCE_TIMEOUT);
    match tokio::time::timeout(BALANCE_TIMEOUT, call).await {
        Ok(Ok(b)) => dto.balance = Some(b),
        Ok(Err(e)) => dto.error = Some(as_field(&e)),
        Err(_) => dto.error = Some(timed_out(BALANCE_TIMEOUT.as_secs())),
    }
    Json(dto)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Без ключа роуты НЕ ХОДЯТ В СЕТЬ и не притворяются сломанными.
    ///
    /// Разница «ключа нет» и «сервис молчит» видна в теле: в первом случае
    /// `error` пуст. Вкладка настроек по этому различию решает, показать
    /// «впишите ключ» или «polza.ai не отвечает».
    #[tokio::test]
    async fn without_a_key_both_routes_answer_empty_and_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let state = MeetState::new(crate::testing::hermetic_config(dir.path()));

        let Json(m) = models(State(state.clone())).await;
        assert!(!m.configured);
        assert!(m.stt.is_empty() && m.chat.is_empty());
        assert_eq!(m.total, 0);
        assert!(m.error.is_none(), "«ключа нет» — не ошибка сервиса: {:?}", m.error);

        let Json(b) = balance(State(state)).await;
        assert!(!b.configured);
        assert!(b.balance.is_none() && b.key.is_none() && b.error.is_none());
    }

    /// Недоступный сервис становится ПОЛЕМ, а не отказом роута.
    ///
    /// Адрес заведомо мёртвый: порт 1 на loopback. Ответ обязан прийти 200 с
    /// заполненным `error`, иначе вкладка настроек умирала бы ровно тогда,
    /// когда её открывают чинить.
    #[tokio::test]
    async fn a_dead_service_becomes_a_field_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        // База именуется отдельной переменной не для красоты: инвариант
        // герметичности (`tests/invariants.rs`) узнаёт только базу, начинающуюся
        // с `hermetic_config(` либо с имени `cfg`/`base`, а путь к модулю перед
        // вызовом он читает как чужой конфиг.
        let base = crate::testing::hermetic_config(dir.path());
        let cfg = crate::config::MeetConfig {
            cloud_base_url: "http://127.0.0.1:1/api/v1".to_string(),
            polza_api_key: crate::cloud::SecretKey::new("pza_test_key_0123456789"),
            ..base
        };
        let state = MeetState::new(cfg);

        let Json(m) = models(State(state.clone())).await;
        assert!(m.configured);
        assert!(m.error.is_some(), "молчащий сервис обязан быть виден полем");
        assert!(m.stt.is_empty() && m.chat.is_empty());

        let Json(b) = balance(State(state)).await;
        assert!(b.configured);
        assert!(b.error.is_some());
        assert!(b.balance.is_none());
    }
}
