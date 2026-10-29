//! Разделяемое состояние плагина `forge-tracker`.
//!
//! Всё, что HTTP-роуты и WebSocket знают о плагине, живёт здесь: хранилище,
//! broadcast-канал событий, реестр фоновых воркеров и общий сигнал остановки.
//! Состояние создаётся один раз в [`crate::init`] и дальше ходит по коду
//! только как `Arc<TrackerState>` — ровно как `Arc<VkState>` в `forge-vk`.
//!
//! # Почему стор синхронный, а состояние асинхронное
//!
//! [`crate::store::TrackerStore`] построен на `std::sync::RwLock`: операции
//! над плоским JSON занимают микросекунды, и городить асинхронный замок ради
//! них означало бы платить за каждую мутацию точкой await без единой выгоды.
//! Асинхронными в состоянии остаются только те части, которые ДЕЙСТВИТЕЛЬНО
//! ждут: broadcast-канал WS и список ручек воркеров.
//!
//! # Broadcast-канал
//!
//! Буфер — [`EVENT_BUFFER`] = 64 сообщения, как у `/ws/todos` хоста. Число не
//! случайное: оно должно переживать пакетную операцию UI (перетащили десяток
//! задач между колонками канбана), но не превращаться в очередь, которая
//! часами хранит устаревшие события для отвалившейся вкладки. Клиент,
//! отставший больше чем на буфер, получает не разрыв, а
//! [`crate::ws::TrackerEvent::Reload`] и пересинхронизируется через
//! `GET /api/tracker/state`.
//!
//! Отправка события через [`TrackerState::emit`] НИКОГДА не считается
//! ошибкой: `broadcast::Sender::send` возвращает `Err`, если подписчиков нет
//! вовсе — а это штатное состояние сервера, у которого просто не открыта
//! вкладка трекера.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::TrackerConfig;
use crate::error::Result;
use crate::store::TrackerStore;
use crate::ws::TrackerEvent;

/// Ёмкость broadcast-канала событий трекера.
pub const EVENT_BUFFER: usize = 64;

/// Состояние плагина: стор, канал событий, воркеры и сигнал остановки.
#[derive(Debug)]
pub struct TrackerState {
    /// Хранилище задач, чек-листов, вех и отрезков времени.
    pub store: TrackerStore,
    /// Отправитель broadcast-канала `/ws/tracker`.
    pub events: broadcast::Sender<TrackerEvent>,
    /// Общий сигнал остановки для фоновых воркеров.
    pub shutdown: CancellationToken,
    /// Ручки запущенных воркеров — их гасит [`crate::shutdown`].
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl TrackerState {
    /// Поднимает состояние из конфига: открывает стор и создаёт канал.
    ///
    /// # Errors
    ///
    /// Только фатальные беды файловой системы. Отсутствующий и даже битый
    /// `tracker.json` ошибкой НЕ считаются (см. [`TrackerStore::load`]):
    /// пользователь с испорченным файлом должен получить работающий сервер и
    /// пустую вкладку, а не отказ старта.
    pub fn new(config: &TrackerConfig) -> Result<Arc<Self>> {
        let store = TrackerStore::open(config)?;
        let (events, _rx) = broadcast::channel(EVENT_BUFFER);
        Ok(Arc::new(Self {
            store,
            events,
            shutdown: CancellationToken::new(),
            workers: Mutex::new(Vec::new()),
        }))
    }

    /// Публикует событие подписчикам `/ws/tracker`.
    ///
    /// Отсутствие подписчиков — не ошибка, а норма (вкладка закрыта), поэтому
    /// результат намеренно проглатывается.
    pub fn emit(&self, event: TrackerEvent) {
        let _ = self.events.send(event);
    }

    /// Регистрирует ручку воркера, чтобы [`crate::shutdown`] мог его погасить.
    pub async fn register_worker(&self, handle: JoinHandle<()>) {
        self.workers.lock().await.push(handle);
    }

    /// Гасит всех воркеров.
    ///
    /// Сначала список ЗАБИРАЕТСЯ из-под замка целиком, и только потом ручки
    /// абортятся: иначе `shutdown` держал бы замок всё время ожидания и
    /// параллельный `register_worker` встал бы на нём намертво. Тот же приём,
    /// что и в `forge-vk` — там за него уже платили (реестр воркеров копил
    /// ручки).
    pub async fn shutdown_workers(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut guard = self.workers.lock().await;
            std::mem::take(&mut *guard)
        };
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
    }
}
