//! Мост между синхронным [`MeetSink`] и асинхронным миром.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Зачем нужен мост
//!
//! [`MeetSink::emit`] синхронен: его зовут из цикла разбора аудиокадров, где
//! `await` означал бы точку отмены посреди дописывания WAV. Но всё, что с
//! событиями надо делать по-настоящему — писать в SQLite, вещать в WS,
//! дописывать `events.ndjson` — асинхронно либо блокирует.
//!
//! [`EventFanout`] решает это единственным честным способом:
//! `emit` кладёт событие в НЕОГРАНИЧЕННУЮ очередь и возвращается за
//! наносекунды, а разбором занимается отдельная задача.
//!
//! # Почему очередь неограниченная
//!
//! Ограниченная очередь в этом месте означает выбор из двух зол: `send().await`
//! (запрещён — `emit` синхронен) или `try_send` с потерей события. Терять
//! события нельзя: `events.ndjson` — единственный способ через сутки понять,
//! почему таймлайн разъехался.
//!
//! Риск неограниченной очереди — рост памяти, если потребитель встал. Он здесь
//! мал по построению: события ЖИЗНЕННОГО ЦИКЛА (не аудиокадры) идут единицами
//! в секунду даже на бурной встрече, а сами кадры PCM в очередь не попадают —
//! они уходят прямо в писателя WAV.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::event::{MeetEvent, MeetSink};

/// Синхронный отправитель в асинхронную очередь.
///
/// Клонируется свободно: все клоны пишут в одну очередь.
#[derive(Debug, Clone)]
pub struct EventFanout {
    tx: mpsc::UnboundedSender<MeetEvent>,
    /// Сколько событий потеряно из-за закрытого приёмника. Ноль при штатной
    /// работе; ненулевое значение означает, что воркер записи умер раньше
    /// источника событий, и это надо видеть, а не гадать.
    lost: Arc<AtomicU64>,
}

/// Приёмная сторона.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<MeetEvent>,
}

impl EventFanout {
    /// Создаёт пару «отправитель — поток».
    pub fn new() -> (Self, EventStream) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                lost: Arc::new(AtomicU64::new(0)),
            },
            EventStream { rx },
        )
    }

    /// Сколько событий не доехало (приёмник закрыт).
    pub fn lost(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// Закрыт ли приёмник.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

impl MeetSink for EventFanout {
    fn emit(&self, event: MeetEvent) {
        if self.tx.send(event).is_err() {
            // Паниковать здесь нельзя: паника в `emit` убила бы задачу записи
            // вместе с недописанным WAV — то есть цену ошибки заплатил бы
            // пользователь, а не автор кода.
            let n = self.lost.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 {
                tracing::warn!(
                    target: "forge_meet",
                    "forge-meet: приёмник событий закрыт, события теряются"
                );
            }
        }
    }
}

impl EventStream {
    /// Следующее событие. `None` — все отправители уничтожены.
    pub async fn recv(&mut self) -> Option<MeetEvent> {
        self.rx.recv().await
    }

    /// Забирает всё, что уже лежит в очереди, не дожидаясь нового.
    pub fn drain(&mut self) -> Vec<MeetEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Закрывает приём: отправители узнают об этом по [`EventFanout::is_closed`].
    pub fn close(&mut self) {
        self.rx.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::event::StopReason;

    #[tokio::test]
    async fn emit_is_synchronous_and_the_stream_sees_order() {
        let (tx, mut stream) = EventFanout::new();
        tx.emit(MeetEvent::Queued);
        tx.emit(MeetEvent::Launching);
        tx.emit(MeetEvent::Joined);

        assert_eq!(stream.recv().await, Some(MeetEvent::Queued));
        assert_eq!(stream.recv().await, Some(MeetEvent::Launching));
        assert_eq!(stream.recv().await, Some(MeetEvent::Joined));
    }

    /// Ключевое свойство: `emit` не имеет права ждать. Тысяча событий подряд
    /// из синхронного контекста обязана уйти мгновенно.
    #[tokio::test]
    async fn a_thousand_events_do_not_block_the_hot_loop() {
        let (tx, mut stream) = EventFanout::new();
        let started = std::time::Instant::now();
        for i in 0..1000u64 {
            tx.emit(MeetEvent::SpeechStarted {
                track_id: "a".into(),
                at_ms: i,
            });
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "1000 emit заняли {:?} — очередь где-то ждёт",
            started.elapsed()
        );
        assert_eq!(stream.drain().len(), 1000);
    }

    /// Мёртвый приёмник не имеет права уронить запись: события считаются
    /// потерянными, но `emit` возвращается нормально.
    #[tokio::test]
    async fn a_dead_receiver_never_panics_the_recorder() {
        let (tx, stream) = EventFanout::new();
        drop(stream);
        tx.emit(MeetEvent::RecordingStopped {
            reason: StopReason::Shutdown,
            at_ms: 1,
        });
        tx.emit(MeetEvent::Queued);
        assert_eq!(tx.lost(), 2);
        assert!(tx.is_closed());
    }

    #[tokio::test]
    async fn clones_share_one_queue() {
        let (tx, mut stream) = EventFanout::new();
        let second = tx.clone();
        tx.emit(MeetEvent::Queued);
        second.emit(MeetEvent::Launching);
        assert_eq!(stream.drain().len(), 2);
    }
}
