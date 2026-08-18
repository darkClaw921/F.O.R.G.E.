//! Писатель WAV **по сэмпловому смещению**.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Почему адрес, а не поток
//!
//! Обычный писатель дописывает в конец: пришёл кадр — добавили. Такой писатель
//! зависит от ПОРЯДКА, и одна потеря сдвигает весь остаток записи влево. На
//! часовой встрече это означает, что транскрипт последних сорока минут не
//! совпадает со звуком, а понять это можно только на слух.
//!
//! Здесь кадр адресуется абсолютным смещением:
//!
//! ```text
//! позиция в файле = 44 (заголовок) + offset_samples * 2
//! ```
//!
//! * кадр пришёл не по порядку — `seek` и запись;
//! * кадр потерян — на его месте ноли, то есть тишина, а таймлайн НЕ СДВИНУТ;
//! * кадр пришёл дважды — перезаписал сам себя теми же байтами.
//!
//! Ноли берутся бесплатно: файловая система отдаёт нулями «дыру», оставленную
//! `seek` за конец файла.
//!
//! # Заголовок чинится по размеру файла
//!
//! Длина в заголовке WAV известна только в конце, а конец может не наступить:
//! Ctrl-C по devforge, отключение питания, паника воркера. Поэтому
//! [`WavWriter::finish`] дописывает правильные длины при штатном закрытии, а
//! [`repair_header`] — восстанавливает их по фактическому размеру файла при
//! разборе прерванной записи. Второй путь не запасной, а основной для
//! `reconcile_on_boot`: именно на нём стоит спасение сорока минут звука.
//!
//! # Чанки пишутся СРАЗУ
//!
//! Ни буфера в памяти, ни отложенного флаша. Это цена, которую мы платим за
//! возможность спасти прерванную запись: то, что не дошло до диска, спасать
//! нечем.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Размер канонического заголовка WAV (RIFF + fmt + data), байт.
pub const WAV_HEADER_LEN: u64 = 44;

/// Сколько байт занимает один сэмпл PCM16 моно.
pub const BYTES_PER_SAMPLE: u64 = 2;

/// Писатель одной дорожки.
#[derive(Debug)]
pub struct WavWriter {
    path: PathBuf,
    file: File,
    sample_rate: u32,
    /// Наибольшее записанное смещение конца, в сэмплах. Оно же длина файла.
    end_samples: u64,
    /// Сколько кадров записано.
    frames: u64,
    /// Сколько кадров пришло не по возрастанию смещения.
    out_of_order: u64,
}

impl WavWriter {
    /// Создаёт файл и пишет заголовок с нулевыми длинами.
    ///
    /// Заголовок пишется СРАЗУ, а не в конце: файл, оборванный на первой
    /// секунде, всё равно обязан быть распознаваемым как WAV — иначе разбор
    /// прерванной записи начинался бы с угадывания формата.
    pub fn create(path: impl Into<PathBuf>, sample_rate: u32) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(&header_bytes(sample_rate, 0))?;
        file.flush()?;
        Ok(Self {
            path,
            file,
            sample_rate,
            end_samples: 0,
            frames: 0,
            out_of_order: 0,
        })
    }

    /// Путь файла.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Записывает кадр ПО ЕГО СОБСТВЕННОМУ смещению.
    pub fn write_frame(&mut self, offset_samples: u64, samples: &[i16]) -> std::io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let end = offset_samples + samples.len() as u64;
        if end <= self.end_samples {
            self.out_of_order += 1;
        }

        let pos = WAV_HEADER_LEN + offset_samples * BYTES_PER_SAMPLE;
        self.file.seek(SeekFrom::Start(pos))?;
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        // Флаш на каждом кадре, а не по расписанию: спасать после Ctrl-C можно
        // только то, что уже на диске.
        self.file.flush()?;

        self.end_samples = self.end_samples.max(end);
        self.frames += 1;
        Ok(())
    }

    /// Сколько сэмплов покрывает файл (включая дырки от потерянных кадров).
    pub fn end_samples(&self) -> u64 {
        self.end_samples
    }

    /// Длительность файла, мс.
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.end_samples * 1000 / u64::from(self.sample_rate)
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Сколько кадров пришло не по возрастанию смещения. Диагностика: само по
    /// себе это норма, но всплеск означает проблему с транспортом.
    pub fn out_of_order(&self) -> u64 {
        self.out_of_order
    }

    /// Дописывает правильные длины и закрывает файл.
    pub fn finish(mut self) -> std::io::Result<u64> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file
            .write_all(&header_bytes(self.sample_rate, self.end_samples * BYTES_PER_SAMPLE))?;
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(self.end_samples)
    }
}

/// Канонический 44-байтный заголовок WAV для PCM16 моно.
fn header_bytes(sample_rate: u32, data_len: u64) -> Vec<u8> {
    let data_len = data_len.min(u64::from(u32::MAX) - WAV_HEADER_LEN) as u32;
    let byte_rate = sample_rate * 2; // моно, 16 бит
    let mut h = Vec::with_capacity(WAV_HEADER_LEN as usize);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // размер fmt
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // каналов
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // бит на сэмпл
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// Чинит заголовок оборванного файла по его ФАКТИЧЕСКОМУ размеру.
///
/// На этой функции стоит спасение прерванной записи: после Ctrl-C в заголовке
/// нули, а на диске сорок минут звука. Возвращает число сэмплов.
///
/// Файл короче заголовка — не ошибка, а пустая запись: возвращается ноль, и
/// вызывающий решает, стоит ли её спасать (порог `salvage_min_secs`).
pub fn repair_header(path: &Path, sample_rate: u32) -> std::io::Result<u64> {
    let len = std::fs::metadata(path)?.len();
    if len <= WAV_HEADER_LEN {
        return Ok(0);
    }
    let data_len = len - WAV_HEADER_LEN;
    // Нечётный хвост — половина сэмпла, то есть оборванная на середине запись.
    // Отбрасываем: половина сэмпла это щелчок, а не звук.
    let samples = data_len / BYTES_PER_SAMPLE;
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header_bytes(sample_rate, samples * BYTES_PER_SAMPLE))?;
    file.flush()?;
    Ok(samples)
}

/// Читает сэмплы файла целиком. Для тестов и для контроля дрейфа.
pub fn read_samples(path: &Path) -> std::io::Result<Vec<i16>> {
    let mut file = File::open(path)?;
    let mut all = Vec::new();
    file.read_to_end(&mut all)?;
    if all.len() as u64 <= WAV_HEADER_LEN {
        return Ok(Vec::new());
    }
    Ok(all[WAV_HEADER_LEN as usize..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

/// Длина данных, записанная В ЗАГОЛОВКЕ (не фактическая). Нужна тестам
/// починки: именно расхождение заголовка и размера файла и есть признак
/// оборванной записи.
pub fn header_data_len(path: &Path) -> std::io::Result<u32> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; WAV_HEADER_LEN as usize];
    file.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes([buf[40], buf[41], buf[42], buf[43]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, v: i16) -> Vec<i16> {
        vec![v; n]
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ: порядок прихода кадров не влияет ни на один байт.
    #[test]
    fn shuffled_frames_give_a_file_identical_to_ordered_ones() {
        let dir = tempfile::tempdir().unwrap();
        let frames: Vec<(u64, Vec<i16>)> = (0..10)
            .map(|i| (i * 320, tone(320, (i as i16 + 1) * 100)))
            .collect();

        let straight = dir.path().join("straight.wav");
        let mut w = WavWriter::create(&straight, 16_000).unwrap();
        for (off, pcm) in &frames {
            w.write_frame(*off, pcm).unwrap();
        }
        w.finish().unwrap();

        // Перемешиваем детерминированно: обратный порядок плюс перестановка
        // соседей — этого достаточно, чтобы поймать любую зависимость от
        // порядка.
        let shuffled_path = dir.path().join("shuffled.wav");
        let mut w = WavWriter::create(&shuffled_path, 16_000).unwrap();
        let mut order: Vec<usize> = (0..frames.len()).rev().collect();
        order.swap(0, 3);
        order.swap(5, 7);
        for i in order {
            let (off, pcm) = &frames[i];
            w.write_frame(*off, pcm).unwrap();
        }
        w.finish().unwrap();

        assert_eq!(
            std::fs::read(&straight).unwrap(),
            std::fs::read(&shuffled_path).unwrap(),
            "порядок прихода кадров изменил файл"
        );
    }

    /// ПОТЕРЯННЫЙ КАДР оставляет ровно столько тишины, сколько занимал, и НЕ
    /// сдвигает последующие сэмплы.
    #[test]
    fn a_lost_frame_leaves_exactly_its_own_silence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gap.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();

        w.write_frame(0, &tone(320, 1000)).unwrap();
        // Кадр на смещении 320 «потерялся».
        w.write_frame(640, &tone(320, 3000)).unwrap();
        w.finish().unwrap();

        let samples = read_samples(&path).unwrap();
        assert_eq!(samples.len(), 960, "длина файла обязана покрывать дырку");
        assert!(samples[..320].iter().all(|&s| s == 1000));
        assert!(
            samples[320..640].iter().all(|&s| s == 0),
            "на месте потерянного кадра обязана быть тишина"
        );
        assert!(
            samples[640..].iter().all(|&s| s == 3000),
            "сэмплы после дырки СДВИНУЛИСЬ — таймлайн уехал"
        );
    }

    /// Дубль кадра перезаписывает сам себя теми же байтами.
    #[test]
    fn a_duplicate_frame_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write_frame(0, &tone(320, 500)).unwrap();
        w.write_frame(320, &tone(320, 700)).unwrap();
        w.write_frame(0, &tone(320, 500)).unwrap();
        assert_eq!(w.out_of_order(), 1, "дубль обязан быть виден в диагностике");
        w.finish().unwrap();

        let samples = read_samples(&path).unwrap();
        assert_eq!(samples.len(), 640);
        assert!(samples[..320].iter().all(|&s| s == 500));
        assert!(samples[320..].iter().all(|&s| s == 700));
    }

    /// ОБРЫВ НА СЕРЕДИНЕ: заголовок с нулями чинится по размеру файла.
    #[test]
    fn an_interrupted_recording_is_readable_after_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.wav");

        // Пишем и НЕ зовём finish — так выглядит Ctrl-C по devforge.
        {
            let mut w = WavWriter::create(&path, 16_000).unwrap();
            for i in 0..50 {
                w.write_frame(i * 320, &tone(320, 1234)).unwrap();
            }
            // `w` уходит из области видимости без finish.
        }

        assert_eq!(
            header_data_len(&path).unwrap(),
            0,
            "заголовок оборванного файла обязан быть пустым — иначе чинить нечего"
        );

        let samples = repair_header(&path, 16_000).unwrap();
        assert_eq!(samples, 50 * 320);
        assert_eq!(header_data_len(&path).unwrap(), (50 * 320 * 2) as u32);

        let data = read_samples(&path).unwrap();
        assert_eq!(data.len(), 50 * 320);
        assert!(data.iter().all(|&s| s == 1234));
    }

    /// Обрыв на ПОЛОВИНЕ сэмпла: половина отбрасывается, файл остаётся
    /// валидным.
    #[test]
    fn a_half_written_sample_is_dropped_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.wav");
        {
            let mut w = WavWriter::create(&path, 16_000).unwrap();
            w.write_frame(0, &tone(10, 7)).unwrap();
        }
        // Дописываем один байт — так выглядит обрыв посреди сэмпла.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[0xAB]).unwrap();
        }
        let samples = repair_header(&path, 16_000).unwrap();
        assert_eq!(samples, 10, "половина сэмпла обязана быть отброшена");
        assert_eq!(read_samples(&path).unwrap().len(), 10);
    }

    /// Пустая запись — не ошибка: вызывающий сам решит, спасать ли её.
    #[test]
    fn an_empty_file_repairs_to_zero_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        WavWriter::create(&path, 16_000).unwrap();
        assert_eq!(repair_header(&path, 16_000).unwrap(), 0);
        assert!(read_samples(&path).unwrap().is_empty());
    }

    /// Заголовок соответствует канону: 16 кГц, моно, 16 бит.
    #[test]
    fn the_header_says_16k_mono_16bit() {
        let h = header_bytes(16_000, 1_000);
        assert_eq!(&h[..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([h[20], h[21]]), 1, "формат PCM");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 1, "моно");
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 16_000);
        assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 32_000, "байт в секунду");
        assert_eq!(u16::from_le_bytes([h[34], h[35]]), 16, "бит на сэмпл");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(h.len() as u64, WAV_HEADER_LEN);
    }

    /// Смещение переводится в позицию файла однозначно и с учётом заголовка.
    #[test]
    fn the_first_sample_lands_right_after_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pos.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write_frame(0, &[0x0102]).unwrap();
        w.finish().unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(raw.len() as u64, WAV_HEADER_LEN + 2);
        assert_eq!(&raw[44..46], &[0x02, 0x01], "little-endian сразу за заголовком");
    }

    /// Дорожка, начинающаяся на пятнадцатой минуте, пишется СО СМЕЩЕНИЕМ:
    /// файл начинается тишиной, и на общей оси звук стоит там, где надо.
    #[test]
    fn a_late_track_starts_with_silence_not_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        let start = 16_000 * 5; // пять секунд
        w.write_frame(start, &tone(320, 999)).unwrap();
        w.finish().unwrap();

        let samples = read_samples(&path).unwrap();
        assert_eq!(samples.len() as u64, start + 320);
        assert!(samples[..start as usize].iter().all(|&s| s == 0));
        assert!(samples[start as usize..].iter().all(|&s| s == 999));
        assert_eq!(
            (start + 320) * 1000 / 16_000,
            5_020,
            "на общей оси дорожка кончается на 5020 мс"
        );
    }
}
