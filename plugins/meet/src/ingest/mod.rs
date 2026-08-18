//! Приём звука: писатель WAV по сэмпловому смещению, `events.ndjson`,
//! `manifest.json`.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! Реализуется в Ф3 (`forge-meet-f3-uz8j.7`). Зафиксированные здесь решения:
//!
//! **Адресация СЭМПЛОВЫМ СМЕЩЕНИЕМ, а не порядком.** Кадр пришёл не по
//! порядку — кладём по смещению; кадр потерян — там остаётся тишина, и
//! таймлайн НЕ СДВИГАЕТСЯ. Это и есть причина выбрать PCM вместо Opus/WebM:
//! `MediaRecorder` с `timeslice` кладёт EBML-заголовок только в первый чанк,
//! поэтому потеря одного чанка делает невалидным весь хвост файла, а вся
//! обвязка вокруг этого (reorder-буфер, `seq`, ротация сегментов, backpressure
//! с ack) существует исключительно ради хрупкости контейнера.
//!
//! **Чанки пишутся на диск СРАЗУ, а не копятся в памяти.** Ровно на этом стоит
//! [`crate::reconcile`]: после Ctrl-C на сороковой минуте спасать есть что.
//!
//! **`events.ndjson` — сырой аудит кадров.** Единственный способ постфактум
//! разобрать, почему таймлайн разъехался, не имея самой встречи.

pub mod journal;
pub mod manifest;
pub mod recorder;
pub mod tracks;
pub mod wav;

pub use journal::EventJournal;
pub use manifest::Manifest;
pub use recorder::{Recorder, RecorderReport};
pub use tracks::{TrackRegistry, TrackState};
pub use wav::WavWriter;
