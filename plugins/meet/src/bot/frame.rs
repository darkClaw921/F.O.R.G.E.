//! Кадр звука на общей оси времени.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Почему адрес кадра — СЭМПЛОВОЕ СМЕЩЕНИЕ, а не порядковый номер
//!
//! Это разрешение единственного противоречия между вариантами транспорта, и
//! оно определяет весь ingest.
//!
//! `MediaRecorder` с `timeslice` кладёт EBML-заголовок только в ПЕРВЫЙ чанк:
//! потеря одного чанка делает невалидным весь хвост файла. Обвязка вокруг этой
//! хрупкости — reorder-буфер, `seq`, ротация сегментов, backpressure с ack,
//! флаг «первый чанк сегмента» — это несколько сотен строк, существующих
//! исключительно потому, что контейнер хрупкий.
//!
//! PCM с абсолютным смещением убирает всю обвязку разом:
//!
//! * кадр пришёл не по порядку — кладём его по своему смещению;
//! * кадр потерян — на его месте остаётся тишина, **таймлайн не сдвигается**;
//! * дублирующийся кадр перезаписывает сам себя теми же байтами.
//!
//! Цена — 32 КБ/с на трек (43 КБ/с после base64). Пять активных дорожек это
//! около 213 КБ/с по локальному сокету, то есть 1,7 Мбит/с JSON на петле.
//!
//! # 16 кГц моно — не произвол
//!
//! Ровно столько хочет whisper. Совпадение частот убирает ffmpeg с
//! КРИТИЧЕСКОГО ПУТИ расшифровки: без ffmpeg не будет только архивного микса,
//! а транскрипт получится.

use serde::{Deserialize, Serialize};

/// Кадр пер-трекового PCM16 моно.
///
/// `pcm` хранится как `i16`, а не как байты: writer'у нужны сэмплы, а
/// перегонять туда-обратно на каждом кадре при 250 кадрах в секунду — лишняя
/// работа на горячем пути.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFrame {
    /// Идентификатор трека платформы (`MediaStreamTrack.id`).
    pub track_id: String,
    /// **Абсолютное** смещение первого сэмпла кадра от `T0`, в сэмплах.
    ///
    /// Именно абсолютное: относительное смещение потребовало бы знать, что
    /// пришло раньше, а это ровно та зависимость от порядка, от которой мы и
    /// уходим.
    pub offset_samples: u64,
    /// Частота дискретизации. Всегда 16 000 в боевом пути; поле есть, чтобы
    /// расхождение было видно в тесте, а не проявилось ускоренной речью.
    pub sample_rate: u32,
    /// Сэмплы, моно.
    pub pcm: Vec<i16>,
}

impl AudioFrame {
    /// Кадр из готовых сэмплов.
    pub fn new(track_id: impl Into<String>, offset_samples: u64, sample_rate: u32, pcm: Vec<i16>) -> Self {
        Self {
            track_id: track_id.into(),
            offset_samples,
            sample_rate,
            pcm,
        }
    }

    /// Сколько сэмплов в кадре.
    pub fn len(&self) -> usize {
        self.pcm.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pcm.is_empty()
    }

    /// Смещение НАЧАЛА кадра на оси времени, мс.
    pub fn start_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.offset_samples * 1000 / u64::from(self.sample_rate)
    }

    /// Смещение КОНЦА кадра, мс.
    pub fn end_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return self.start_ms();
        }
        (self.offset_samples + self.pcm.len() as u64) * 1000 / u64::from(self.sample_rate)
    }

    /// Байтовое смещение в WAV-файле трека (без учёта заголовка).
    ///
    /// Здесь и живёт вся идея адресации: позиция в файле выводится ИЗ КАДРА, а
    /// не из того, сколько байт было записано раньше.
    pub fn byte_offset(&self) -> u64 {
        self.offset_samples * 2
    }

    /// Little-endian PCM16 — ровно то, что уходит в файл.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pcm.len() * 2);
        for s in &self.pcm {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    /// Разбор little-endian PCM16. Нечётный хвост отбрасывается: половина
    /// сэмпла — это испорченный кадр, а не половина звука.
    pub fn samples_from_le_bytes(bytes: &[u8]) -> Vec<i16> {
        bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    /// Среднеквадратичный уровень кадра, 0.0..1.0. Фолбэк VAD для тапа Web
    /// Audio, где `getSynchronizationSources().audioLevel` недоступен.
    pub fn rms(&self) -> f32 {
        if self.pcm.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.pcm.iter().map(|&s| {
            let v = f64::from(s) / f64::from(i16::MAX);
            v * v
        }).sum();
        (sum / self.pcm.len() as f64).sqrt() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_addresses_itself_by_sample_offset() {
        let f = AudioFrame::new("t1", 16_000, 16_000, vec![0; 320]);
        assert_eq!(f.start_ms(), 1_000, "16 000 сэмплов при 16 кГц — ровно секунда");
        assert_eq!(f.end_ms(), 1_020, "320 сэмплов это 20 мс");
        assert_eq!(f.byte_offset(), 32_000, "два байта на сэмпл");
    }

    /// Круговой ход байтов: то, что улетело в файл, читается обратно тем же.
    #[test]
    fn pcm_survives_the_round_trip_through_bytes() {
        let pcm = vec![0i16, 1, -1, i16::MAX, i16::MIN, 12345];
        let f = AudioFrame::new("t1", 0, 16_000, pcm.clone());
        let bytes = f.to_le_bytes();
        assert_eq!(bytes.len(), pcm.len() * 2);
        assert_eq!(AudioFrame::samples_from_le_bytes(&bytes), pcm);
    }

    /// Испорченный кадр не имеет права сдвинуть все последующие сэмплы на
    /// полбайта.
    #[test]
    fn an_odd_tail_is_dropped_not_padded() {
        assert_eq!(AudioFrame::samples_from_le_bytes(&[1, 0, 2]), vec![1i16]);
        assert_eq!(AudioFrame::samples_from_le_bytes(&[7]), Vec::<i16>::new());
    }

    #[test]
    fn rms_is_zero_for_silence_and_grows_with_amplitude() {
        let quiet = AudioFrame::new("t", 0, 16_000, vec![0; 100]);
        assert_eq!(quiet.rms(), 0.0);
        let loud = AudioFrame::new("t", 0, 16_000, vec![i16::MAX; 100]);
        assert!((loud.rms() - 1.0).abs() < 1e-3, "{}", loud.rms());
        let mid = AudioFrame::new("t", 0, 16_000, vec![i16::MAX / 2; 100]);
        assert!(mid.rms() < loud.rms() && mid.rms() > quiet.rms());
    }

    #[test]
    fn a_zero_sample_rate_does_not_divide_by_zero() {
        let f = AudioFrame::new("t", 100, 0, vec![0; 10]);
        assert_eq!(f.start_ms(), 0);
        assert_eq!(f.end_ms(), 0);
    }
}
