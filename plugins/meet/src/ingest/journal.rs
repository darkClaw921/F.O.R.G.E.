//! `events.ndjson` — сырой аудит записи.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Зачем он есть, если события уже уходят в WS и в базу
//!
//! Потому что через сутки, разбирая «почему у этой встречи транскрипт
//! разъехался», в руках оказывается ТОЛЬКО каталог. WS-кадры ушли в закрытую
//! вкладку, база хранит переходы статусов, но не хранит, когда появилась
//! третья дорожка и сколько кадров потеряла страница до привязки биндинга.
//!
//! Журнал — единственный способ восстановить ход записи постфактум, не имея
//! самой встречи. Одна строка — одно событие, формат NDJSON выбран ровно
//! поэтому: файл читается построчно, и оборванная последняя строка не мешает
//! прочитать все предыдущие.
//!
//! # Дописывается, а не переписывается
//!
//! `OpenOptions::append`. Атомарность здесь не нужна и вредна: цель —
//! сохранить максимум того, что успело произойти, а `tmp + rename` на каждой
//! строке означал бы копирование растущего файла сотни раз за встречу.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Имя файла в каталоге встречи.
pub const JOURNAL_FILE: &str = "events.ndjson";

/// Строка журнала.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Момент на общей оси, мс от `T0`.
    pub at_ms: u64,
    /// Имя события. Совпадает с `MeetEvent::kind()` либо с именем сырого
    /// события страницы.
    pub kind: String,
    /// Полезная нагрузка как есть.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

/// Писатель журнала.
#[derive(Debug)]
pub struct EventJournal {
    path: PathBuf,
    file: File,
    lines: u64,
}

impl EventJournal {
    /// Открывает журнал на дозапись, создавая при необходимости.
    pub fn open(meeting_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(meeting_dir)?;
        let path = meeting_dir.join(JOURNAL_FILE);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let lines = std::fs::read_to_string(&path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0);
        Ok(Self { path, file, lines })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Дописывает событие.
    ///
    /// Ошибка записи журнала НЕ прерывает запись встречи: аудит ценен, но
    /// звук ценнее. Вызывающий логирует и идёт дальше.
    pub fn append(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.lines += 1;
        Ok(())
    }

    /// Короткая форма записи события.
    pub fn note(
        &mut self,
        at_ms: u64,
        kind: &str,
        data: serde_json::Value,
    ) -> std::io::Result<()> {
        self.append(&JournalEntry {
            at_ms,
            kind: kind.to_string(),
            data,
        })
    }

    /// Читает журнал целиком.
    ///
    /// **Оборванная последняя строка пропускается**, а не роняет разбор: она и
    /// есть нормальный вид журнала записи, прерванной по питанию.
    pub fn read(meeting_dir: &Path) -> std::io::Result<Vec<JournalEntry>> {
        let path = meeting_dir.join(JOURNAL_FILE);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&line) {
                Ok(entry) => out.push(entry),
                // Битая строка — конец полезных данных. Дальше идти незачем:
                // файл дописывался последовательно.
                Err(_) => break,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_are_written_line_by_line_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = EventJournal::open(dir.path()).unwrap();
        j.note(0, "recording_started", json!({"unix": 1_700_000_000_000u64})).unwrap();
        j.note(120, "track_added", json!({"t": "a", "idx": 0})).unwrap();
        j.note(1_500, "speech_start", json!({"t": "a"})).unwrap();
        assert_eq!(j.lines(), 3);

        let back = EventJournal::read(dir.path()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].kind, "track_added");
        assert_eq!(back[1].at_ms, 120);
        assert_eq!(back[1].data["idx"], 0);
    }

    /// Журнал ДОПИСЫВАЕТСЯ: перезапуск воркера не имеет права стереть то, что
    /// уже произошло.
    #[test]
    fn reopening_appends_instead_of_truncating() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut j = EventJournal::open(dir.path()).unwrap();
            j.note(0, "a", json!({})).unwrap();
        }
        {
            let mut j = EventJournal::open(dir.path()).unwrap();
            assert_eq!(j.lines(), 1, "существующие строки обязаны быть посчитаны");
            j.note(1, "b", json!({})).unwrap();
        }
        let back = EventJournal::read(dir.path()).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].kind, "a");
        assert_eq!(back[1].kind, "b");
    }

    /// ОБОРВАННАЯ ПОСЛЕДНЯЯ СТРОКА — нормальный вид журнала после отключения
    /// питания. Она не имеет права уронить разбор всех предыдущих.
    #[test]
    fn a_truncated_last_line_does_not_lose_the_previous_ones() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut j = EventJournal::open(dir.path()).unwrap();
            for i in 0..5 {
                j.note(i * 100, "tick", json!({"n": i})).unwrap();
            }
        }
        // Дописываем обрубок — так выглядит обрыв на середине записи строки.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join(JOURNAL_FILE))
                .unwrap();
            f.write_all(br#"{"at_ms":500,"kind":"tic"#).unwrap();
        }

        let back = EventJournal::read(dir.path()).unwrap();
        assert_eq!(back.len(), 5, "целые строки обязаны прочитаться");
        assert_eq!(back[4].data["n"], 4);
    }

    #[test]
    fn a_missing_journal_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(EventJournal::read(dir.path()).unwrap().is_empty());
    }

    /// Тысяча событий часовой встречи не должна ни замедлять запись, ни
    /// разрастаться сверх разумного.
    #[test]
    fn a_thousand_events_stay_small_and_fast() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = EventJournal::open(dir.path()).unwrap();
        let started = std::time::Instant::now();
        for i in 0..1000u64 {
            j.note(i * 60, "speech_start", json!({"t": "track-a"})).unwrap();
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "тысяча строк журнала заняла {:?}",
            started.elapsed()
        );
        let size = std::fs::metadata(j.path()).unwrap().len();
        assert!(size < 200 * 1024, "журнал на 1000 событий весит {size} байт");
        assert_eq!(EventJournal::read(dir.path()).unwrap().len(), 1000);
    }
}
