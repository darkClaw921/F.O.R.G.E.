//! `MeetHub` — оркестрация ресурсов встречи.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Три ресурса с РАЗНЫМ поведением при переполнении
//!
//! | ресурс | лимит | переполнение | почему так |
//! |---|---|---|---|
//! | запись | 2 | `Busy` **немедленно** | встреча идёт ПРЯМО СЕЙЧАС; ожидание слота означает, что бот войдёт, когда всё уже кончилось |
//! | whisper | **1, поднять нельзя** | FIFO-очередь | Metal занимается процессом целиком, два прогона дерутся и суммарно идут медленнее одного |
//! | саммари | 2 | FIFO-очередь | в реальном времени саммари никто не ждёт |
//!
//! Разница между «немедленный отказ» и «очередь» здесь — не настройка, а суть:
//! это единственное место, где ожидание невозможно восполнить позже.
//!
//! # Очередь видна честно
//!
//! Ждущая расшифровка остаётся в статусе `transcribing`, но кадр `progress`
//! несёт `queued: true` и `queue_position`. Отдельного статуса «в очереди»
//! нет: он размножил бы таблицу переходов вдвое ради временной информации.
//!
//! Ф2 дала слоты и учёт активных записей, Ф3 — [`MeetEvent`], [`MeetSink`] с
//! единственным методом `emit` (разбор формы трейта — в шапке [`event`]) и
//! честную позицию в очереди whisper.

pub mod event;
pub mod sink;

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore, TryAcquireError};

use crate::config::MeetConfig;
use crate::error::MeetError;
use crate::ws::protocol::Limits;

pub use event::{
    validate_order, CollectingSink, MeetEvent, MeetSink, NullSink, OrderViolation, ParticipantInfo,
    StopReason, TeeSink,
};
pub use sink::{EventFanout, EventStream};

/// Разрешение на занятый ресурс. Освобождается при `drop` — в том числе при
/// панике воркера, из-за чего слот невозможно потерять навсегда.
pub type Slot = OwnedSemaphorePermit;

/// Ручка оркестратора.
#[derive(Debug)]
pub struct MeetHub {
    recordings: Arc<Semaphore>,
    whisper: Arc<Semaphore>,
    summaries: Arc<Semaphore>,
    limits: Limits,
    /// Идентификаторы встреч, которые сейчас пишутся. Нужны, чтобы снапшот и
    /// `shutdown` знали, кого будить.
    active: RwLock<HashSet<String>>,
    /// Сколько запросов сейчас В ОЧЕРЕДИ whisper — ждущих ПЛЮС тот, кто слот
    /// уже держит. Считается счётчиком, а не через
    /// `Semaphore::available_permits`: последний умеет отвечать только
    /// «свободно/занято» и ничего не знает о длине хвоста, а в кадре
    /// `progress` пользователю нужна именно позиция.
    whisper_waiting: Arc<AtomicUsize>,
    /// То же для саммари: ждущие ПЛЮС держащие слот. Считается счётчиком, а не
    /// через `available_permits`, по той же причине — семафор не знает длины
    /// хвоста, а кадру `progress` нужна позиция.
    summaries_waiting: Arc<AtomicUsize>,
}

/// Слот whisper вместе с местом, которое запрос занимал в очереди.
///
/// Позиция снимается В МОМЕНТ ПОСТАНОВКИ: к моменту выдачи слота она всегда
/// ноль, и показывать её было бы бессмысленно.
///
/// Держит счётчик очереди занятым, пока жив: поле `slot` намеренно приватно —
/// вынь его наружу, и счётчик уехал бы вниз раньше окончания прогона, а
/// следующий в очереди увидел бы позицию 0, стоя за работающим whisper.
#[derive(Debug)]
pub struct QueuedSlot {
    #[allow(dead_code)]
    slot: Slot,
    queue_position: usize,
    /// Уменьшает счётчик очереди при `drop` — в том числе при панике воркера.
    #[allow(dead_code)]
    waiting: WaitingGuard,
}

impl QueuedSlot {
    /// Сколько прогонов стояло впереди в момент постановки. `0` — слот
    /// достался сразу.
    pub fn queue_position(&self) -> usize {
        self.queue_position
    }

    /// Ждал ли запрос очереди. Ровно это уезжает в кадр `progress` полем
    /// `queued`.
    pub fn was_queued(&self) -> bool {
        self.queue_position > 0
    }
}

impl MeetHub {
    pub fn new(cfg: &MeetConfig) -> Arc<Self> {
        let limits = Limits {
            recordings: cfg.max_concurrent_recordings,
            whisper: cfg.max_concurrent_whisper,
            summaries: cfg.max_concurrent_summaries,
        };
        Arc::new(Self {
            recordings: Arc::new(Semaphore::new(limits.recordings)),
            whisper: Arc::new(Semaphore::new(limits.whisper)),
            summaries: Arc::new(Semaphore::new(limits.summaries)),
            limits,
            active: RwLock::new(HashSet::new()),
            whisper_waiting: Arc::new(AtomicUsize::new(0)),
            summaries_waiting: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Пытается занять слот записи. **Не ждёт.**
    ///
    /// `Err(MeetError::Busy)` возвращается за микросекунды — на это есть
    /// требование «`Busy` за <50 мс». Ожидание здесь было бы прямым вредом:
    /// пока запрос висит в очереди, встреча идёт без бота.
    pub fn try_acquire_recording(&self) -> Result<Slot, MeetError> {
        match self.recordings.clone().try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(TryAcquireError::NoPermits) => Err(MeetError::Busy),
            Err(TryAcquireError::Closed) => Err(MeetError::Internal(
                "оркестратор остановлен".to_string(),
            )),
        }
    }

    /// Занимает слот whisper, ЖДЁТ в FIFO-очереди.
    ///
    /// Ожидание здесь законно ровно потому, что расшифровка не имеет дедлайна:
    /// запись уже на диске и никуда не денется.
    pub async fn acquire_whisper(&self) -> Result<QueuedSlot, MeetError> {
        self.acquire_whisper_queued().await
    }

    /// То же, но с позицией в очереди на момент постановки.
    ///
    /// Позиция уезжает в кадр [`crate::ws::protocol::MeetServerMsg::Progress`]
    /// полями `queued: true` и `queue_position`. Отдельного СТАТУСА «в
    /// очереди» нет намеренно: он размножил бы таблицу переходов вдвое ради
    /// информации, которая по природе своей временная.
    ///
    /// Счётчик уменьшается через RAII: `?` на закрытой очереди тоже обязан его
    /// вернуть, иначе после `close()` позиция навсегда осталась бы завышенной.
    pub async fn acquire_whisper_queued(&self) -> Result<QueuedSlot, MeetError> {
        let ahead = self.whisper_waiting.fetch_add(1, Ordering::SeqCst);
        let waiting = WaitingGuard(self.whisper_waiting.clone());
        let slot = self
            .whisper
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| MeetError::Internal("очередь whisper закрыта".to_string()))?;
        Ok(QueuedSlot {
            slot,
            queue_position: ahead,
            waiting,
        })
    }

    /// Сколько прогонов whisper сейчас в очереди, считая работающий.
    pub fn whisper_queue_len(&self) -> usize {
        self.whisper_waiting.load(Ordering::SeqCst)
    }

    /// Занимает слот саммари, ЖДЁТ в FIFO-очереди.
    pub async fn acquire_summary(&self) -> Result<Slot, MeetError> {
        self.summaries
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| MeetError::Internal("очередь саммари закрыта".to_string()))
    }

    /// То же, но с позицией в очереди на момент постановки.
    ///
    /// Позиция уезжает в кадр `progress` полями `queued` и `queue_position` —
    /// ровно как у whisper. Счётчик возвращается через RAII, поэтому `?` на
    /// закрытой очереди тоже его отдаёт.
    pub async fn acquire_summary_queued(&self) -> Result<QueuedSlot, MeetError> {
        let ahead = self.summaries_waiting.fetch_add(1, Ordering::SeqCst);
        let waiting = WaitingGuard(self.summaries_waiting.clone());
        let slot = self
            .summaries
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| MeetError::Internal("очередь саммари закрыта".to_string()))?;
        // Позиция считается ОТ ЧИСЛА СЛОТОВ: при лимите 2 третий запрос стоит
        // за двумя работающими, а первые два не ждали вовсе. Вычитать нужно
        // именно лимит, иначе второй параллельный прогон показывал бы «вы
        // второй в очереди», не простояв в ней ни микросекунды.
        let queue_position = ahead.saturating_sub(self.limits.summaries.saturating_sub(1));
        Ok(QueuedSlot {
            slot,
            queue_position,
            waiting,
        })
    }

    /// Сколько запросов саммари сейчас в очереди, считая работающие.
    pub fn summary_queue_len(&self) -> usize {
        self.summaries_waiting.load(Ordering::SeqCst)
    }

    pub fn summaries_available(&self) -> usize {
        self.summaries.available_permits()
    }

    /// Сколько запросов ждёт слота whisper (позиция в очереди для кадра
    /// `progress`).
    pub fn whisper_available(&self) -> usize {
        self.whisper.available_permits()
    }

    pub fn recordings_available(&self) -> usize {
        self.recordings.available_permits()
    }

    /// Отмечает встречу как пишущуюся.
    pub async fn mark_active(&self, meeting_id: &str) {
        self.active.write().await.insert(meeting_id.to_string());
    }

    pub async fn mark_inactive(&self, meeting_id: &str) {
        self.active.write().await.remove(meeting_id);
    }

    pub async fn active_meetings(&self) -> Vec<String> {
        let mut v: Vec<String> = self.active.read().await.iter().cloned().collect();
        v.sort();
        v
    }

    /// Закрывает семафоры: новые операции больше не начинаются.
    ///
    /// Уже выданные разрешения при этом ОСТАЮТСЯ действительными — идущая
    /// запись обязана дофлашить WAV, а не оборваться на середине.
    pub fn close(&self) {
        self.recordings.close();
        self.whisper.close();
        self.summaries.close();
    }
}

/// Возвращает счётчик очереди при выходе из неё ЛЮБЫМ путём — окончанием
/// прогона, ошибкой закрытой очереди, отменой задачи или паникой воркера.
#[derive(Debug)]
struct WaitingGuard(Arc<AtomicUsize>);

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(recordings: usize) -> Arc<MeetHub> {
        MeetHub::new(&MeetConfig {
            max_concurrent_recordings: recordings,
            ..MeetConfig::default()
        })
    }

    /// Переполнение записи обязано отвечать НЕМЕДЛЕННО: пока запрос ждал бы
    /// очереди, встреча шла бы без бота.
    #[tokio::test]
    async fn recording_overflow_is_immediate_not_queued() {
        let h = hub(2);
        let _a = h.try_acquire_recording().unwrap();
        let _b = h.try_acquire_recording().unwrap();

        let started = std::time::Instant::now();
        // Сравниваем ошибку, а не весь Result: `OwnedSemaphorePermit` не
        // реализует `PartialEq` — и не должен, это не значение, а владение.
        let third = h.try_acquire_recording();
        assert_eq!(third.err(), Some(MeetError::Busy));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "Busy обязан приходить за микросекунды, занял {:?}",
            started.elapsed()
        );
    }

    /// Слот возвращается при `drop`, в том числе если воркер запаниковал.
    #[tokio::test]
    async fn a_dropped_slot_comes_back() {
        let h = hub(1);
        {
            let _slot = h.try_acquire_recording().unwrap();
            assert!(h.try_acquire_recording().is_err());
        }
        assert!(h.try_acquire_recording().is_ok(), "слот не вернулся после drop");
    }

    /// Очередь whisper видна честно: второй прогон получает НЕНУЛЕВУЮ позицию,
    /// а не просто «подождите». Ровно это уезжает в кадр `progress`.
    #[tokio::test]
    async fn the_second_whisper_run_learns_its_place_in_the_queue() {
        let h = hub(2);
        let first = h.acquire_whisper_queued().await.unwrap();
        assert_eq!(first.queue_position(), 0, "первому ждать не пришлось");
        assert!(!first.was_queued());
        assert_eq!(h.whisper_queue_len(), 1);

        let h2 = h.clone();
        let second = tokio::spawn(async move { h2.acquire_whisper_queued().await });
        // Дождёмся, пока второй реально встанет в очередь.
        for _ in 0..100 {
            if h.whisper_queue_len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(h.whisper_queue_len(), 2, "второй не встал в очередь");

        let h3 = h.clone();
        let third = tokio::spawn(async move { h3.acquire_whisper_queued().await });
        for _ in 0..100 {
            if h.whisper_queue_len() == 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        drop(first);
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .expect("второй не проснулся")
            .unwrap()
            .unwrap();
        assert_eq!(second.queue_position(), 1, "второй стоял за одним");
        assert!(second.was_queued());

        drop(second);
        let third = tokio::time::timeout(std::time::Duration::from_secs(2), third)
            .await
            .expect("третий не проснулся")
            .unwrap()
            .unwrap();
        assert_eq!(third.queue_position(), 2, "третий стоял за двумя");
        drop(third);

        // Очередь обязана рассосаться полностью, иначе следующая встреча
        // показала бы несуществующий хвост.
        assert_eq!(h.whisper_queue_len(), 0);
    }

    /// Счётчик очереди возвращается и при ОШИБКЕ, а не только при успехе.
    /// Иначе после `close()` позиция навсегда осталась бы завышенной.
    #[tokio::test]
    async fn a_closed_queue_still_gives_the_counter_back() {
        let h = hub(1);
        h.close();
        assert!(h.acquire_whisper_queued().await.is_err());
        assert_eq!(h.whisper_queue_len(), 0);
    }

    /// Whisper ждёт, а не отказывает: запись уже на диске и никуда не денется.
    #[tokio::test]
    async fn whisper_queues_instead_of_refusing() {
        let h = hub(2);
        assert_eq!(h.limits().whisper, 1, "лимит whisper поднять нельзя");
        let first = h.acquire_whisper().await.unwrap();

        let h2 = h.clone();
        let waiter = tokio::spawn(async move { h2.acquire_whisper().await.map(|_| ()) });
        // Пока слот занят, ожидающий не должен завершиться.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "второй прогон whisper пролез мимо семафора");

        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("ожидающий не проснулся")
            .unwrap()
            .unwrap();
    }

    /// **ДВА САММАРИ ИДУТ ОДНОВРЕМЕННО, ТРЕТЬЕ ЖДЁТ.**
    ///
    /// Ровно то поведение, ради которого у саммари два слота и очередь, а не
    /// немедленный отказ: результата никто не ждёт в реальном времени.
    #[tokio::test]
    async fn two_summaries_run_at_once_and_the_third_waits_in_line() {
        let h = hub(2);
        assert_eq!(h.limits().summaries, 2);

        let first = h.acquire_summary_queued().await.unwrap();
        let second = h.acquire_summary_queued().await.unwrap();
        assert!(!first.was_queued(), "первый ждать не должен");
        assert!(!second.was_queued(), "второй слот свободен — ждать нечего");
        assert_eq!(h.summaries_available(), 0, "оба слота заняты");

        let h3 = h.clone();
        let third = tokio::spawn(async move { h3.acquire_summary_queued().await });
        for _ in 0..100 {
            if h.summary_queue_len() == 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(h.summary_queue_len(), 3, "третий не встал в очередь");
        assert!(!third.is_finished(), "третье саммари пролезло мимо семафора");

        drop(first);
        let third = tokio::time::timeout(std::time::Duration::from_secs(2), third)
            .await
            .expect("третий не проснулся")
            .unwrap()
            .unwrap();
        assert!(third.was_queued(), "третий обязан знать, что стоял в очереди");
        assert_eq!(third.queue_position(), 1, "он стоял за одним лишним");

        drop(second);
        drop(third);
        assert_eq!(h.summary_queue_len(), 0, "очередь саммари не рассосалась");
    }

    /// Даже если кто-то положит в конфиг четвёрку, лимит вернётся к единице
    /// ещё на этапе `validate_and_fix` — а хаб обязан взять именно
    /// исправленное значение.
    #[tokio::test]
    async fn the_hub_never_gets_more_than_one_whisper_slot() {
        let mut cfg = MeetConfig {
            max_concurrent_whisper: 4,
            ..MeetConfig::default()
        };
        let fixes = cfg.validate_and_fix();
        assert!(fixes.iter().any(|f| f.contains("max_concurrent_whisper")));
        let h = MeetHub::new(&cfg);
        assert_eq!(h.limits().whisper, 1);
    }

    #[tokio::test]
    async fn active_meetings_are_tracked_and_sorted() {
        let h = hub(2);
        h.mark_active("b").await;
        h.mark_active("a").await;
        assert_eq!(h.active_meetings().await, vec!["a", "b"]);
        h.mark_inactive("a").await;
        assert_eq!(h.active_meetings().await, vec!["b"]);
    }

    /// Закрытие не отбирает уже выданные разрешения: идущая запись обязана
    /// дофлашить WAV, а не оборваться.
    #[tokio::test]
    async fn closing_does_not_revoke_issued_slots() {
        let h = hub(1);
        let slot = h.try_acquire_recording().unwrap();
        h.close();
        assert!(h.try_acquire_recording().is_err(), "после close новые слоты не выдаются");
        drop(slot); // не паникует
    }
}
