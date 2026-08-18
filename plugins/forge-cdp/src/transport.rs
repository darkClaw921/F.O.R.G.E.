//! [`CdpTransport`] — граница подмены реализации протокола.
//!
//! # Зачем трейт, если реализация одна
//!
//! Крейт написан своими руками поверх `tokio-tungstenite` осознанно
//! (см. `Cargo.toml`: chromiumoxide тянет второй ws-стек и роняет
//! `cargo tree -d`). Но выбор «свой клиент» — это ставка, и у ставки должен
//! быть путь отхода: если flat-сессии, автоаттач в OOPIF или биндинги
//! окажутся неподъёмными, замена на chromiumoxide обязана быть работой
//! ВНУТРИ `forge-cdp`, а не переписыванием `forge-meet`.
//!
//! Поэтому весь остальной крейт — [`crate::session`], [`crate::page`],
//! [`crate::inject`] — работает не с сокетом, а с `Arc<dyn CdpTransport>`.
//! Что означает подмена на практике: нужно реализовать ровно три вещи —
//! отправку команды с ожиданием ответа, широковещательную подписку на события
//! и закрытие; список фактически используемых методов протокола приведён в
//! документации крейта ([`crate`]).
//!
//! Второе следствие, не менее важное: тесты крейта и его потребителей идут
//! против [`crate::testing::RecordingTransport`] и не требуют ни живого
//! Chrome, ни сети. Проверка «в каком порядке ушли команды» — это проверка
//! списка записанных вызовов, а не наблюдение за браузером.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::protocol::{CdpError, CdpEvent, EventFilter};

/// Настройки транспорта.
///
/// Таймаут — ПОЛЕ, а не константа: у `Page.navigate` живой встречи и у
/// `Runtime.evaluate` на мок-странице разумные пределы отличаются на порядок,
/// а зашитая константа неминуемо оказалась бы неверной для одного из них.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Сколько ждать ответа на команду, прежде чем вернуть
    /// [`CdpError::Timeout`].
    pub command_timeout: Duration,
    /// Ёмкость broadcast-канала событий. События CDP на живой встрече идут
    /// потоком (`Runtime.bindingCalled` на каждый кадр PCM), и медленный
    /// подписчик обязан потерять СТАРЫЕ события, а не затормозить роутер:
    /// broadcast именно так и устроен, потерю видно как `RecvError::Lagged`.
    pub event_buffer: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            // 30 с: верхняя граница взята под `Page.navigate` на холодном
            // профиле Chrome с сетевым логином — команда, которая честно
            // может идти секунды. Всё, что быстрее, всё равно вернётся сразу.
            command_timeout: Duration::from_secs(30),
            // 1024 — та же ёмкость, что у broadcast-каналов плагинов devforge.
            event_buffer: 1024,
        }
    }
}

/// Транспорт CDP: отправка команд и подписка на события.
///
/// Реализации: [`crate::ws::WsTransport`] (боевая, `tokio-tungstenite`) и
/// [`crate::testing::RecordingTransport`] (тестовая, без сокета).
#[async_trait]
pub trait CdpTransport: Send + Sync + 'static {
    /// Отправляет команду и ждёт ответа.
    ///
    /// Контракт:
    /// * возвращает `result` целиком (без обёртки);
    /// * ошибка Chrome превращается в [`CdpError::Protocol`] с кодом;
    /// * по истечении [`TransportConfig::command_timeout`] — [`CdpError::Timeout`],
    ///   а не вечное ожидание;
    /// * если сокет закрылся, пока команда в полёте, — [`CdpError::TransportClosed`];
    /// * вызовы конкурентны: N команд, отправленных одновременно, получают
    ///   каждая СВОЙ ответ (роутинг по `id`).
    async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, CdpError>;

    /// Подписка на поток событий всех целей.
    ///
    /// Фильтрация по методу и сессии — на стороне подписчика
    /// ([`EventFilter`]), потому что в flat-режиме канал общий: события
    /// страницы, каждого OOPIF и воркеров идут вперемешку.
    ///
    /// ВАЖЕН ПОРЯДОК: подписываться нужно ДО отправки команды, которая
    /// породит ожидаемое событие. Иначе между `send` и `subscribe` есть окно,
    /// в котором событие уже пришло и потеряно, — а выглядеть это будет как
    /// «иногда зависает».
    fn subscribe(&self) -> broadcast::Receiver<CdpEvent>;

    /// Закрыт ли транспорт (браузер умер или мы закрылись сами).
    fn is_closed(&self) -> bool;

    /// Закрывает соединение и будит все ждущие команды
    /// [`CdpError::TransportClosed`].
    async fn close(&self);
}

/// Ждёт первое событие, подходящее под фильтр, на уже открытой подписке.
///
/// Отдельная функция, а не метод трейта: подписаться нужно ЗАРАНЕЕ (см.
/// [`CdpTransport::subscribe`]), поэтому приёмник передаётся снаружи.
///
/// Отставание подписчика (`Lagged`) не считается ошибкой ожидания: канал
/// специально теряет старые события под нагрузкой. Потеря логируется и
/// ожидание продолжается — иначе всплеск `bindingCalled` во время записи
/// ронял бы совершенно посторонние ожидания.
pub async fn wait_for_event(
    rx: &mut broadcast::Receiver<CdpEvent>,
    filter: &EventFilter,
    timeout: Duration,
) -> Result<CdpEvent, CdpError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(CdpError::Timeout {
                method: describe_filter(filter),
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if event.matches(filter) {
                    return Ok(event);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!(
                    lost = n,
                    filter = %describe_filter(filter),
                    "forge-cdp: подписчик событий отстал, потеряно событий"
                );
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => return Err(CdpError::TransportClosed),
            Err(_) => {
                return Err(CdpError::Timeout {
                    method: describe_filter(filter),
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }
}

fn describe_filter(filter: &EventFilter) -> String {
    match &filter.method {
        Some(m) => format!("событие {m}"),
        None => "любое событие".to_string(),
    }
}

/// Удобный псевдоним: транспорт всегда живёт за `Arc` — им одновременно
/// владеют сессии, насос автоаттача и приёмник биндингов.
pub type SharedTransport = Arc<dyn CdpTransport>;
