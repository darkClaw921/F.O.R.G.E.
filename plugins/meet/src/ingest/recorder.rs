//! [`Recorder`] — приём звука и событий встречи на диск.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Что он делает
//!
//! Собирает вместе четыре вещи, каждая из которых сама по себе проста, но
//! разъезжается, если их не свести в одном месте:
//!
//! | | что |
//! |---|---|
//! | [`super::tracks::TrackRegistry`] | кто под каким номером пишется, и номера НЕ переиспользуются |
//! | [`super::wav::WavWriter`] | по писателю на дорожку, адресация сэмпловым смещением |
//! | [`super::journal::EventJournal`] | сырой аудит: что и когда произошло |
//! | [`super::manifest::Manifest`] | паспорт записи для пост-обработки |
//!
//! # Раскладка на диске
//!
//! ```text
//! ~/.forge/meet/<id>/
//!   manifest.json      паспорт записи (атомарно)
//!   events.ndjson      сырой аудит
//!   raw/track-0.wav    пер-трековый PCM16 16 кГц моно
//!   raw/track-1.wav
//!   chrome.pid         pid браузера (пишет forge-cdp)
//!   work/              транзитные файлы пост-обработки (Ф5)
//!   transcript.json    расшифровка (Ф5)
//!   summary.md         саммари (Ф6)
//! ```
//!
//! # Всё пишется СРАЗУ
//!
//! Ни один кадр не задерживается в памяти дольше одного вызова. Это цена за
//! возможность спасти прерванную запись: `reconcile_on_boot` умеет поднять
//! сорок минут звука после Ctrl-C ровно потому, что эти сорок минут уже лежат
//! на диске. Буфер «для производительности» здесь означал бы потерю последних
//! секунд при каждом падении.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::journal::EventJournal;
use super::manifest::Manifest;
use super::tracks::TrackRegistry;
use super::wav::WavWriter;
use crate::bot::frame::AudioFrame;
use crate::bot::mode::TrackMode;
use crate::bot::transport::{PageEvent, PageMessage, TransportStats};

/// Подкаталог с пер-трековыми WAV.
pub const RAW_DIR: &str = "raw";

/// Итог записи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderReport {
    /// Сколько дорожек было за встречу (включая закрытые).
    pub tracks: usize,
    /// Сколько кадров принято.
    pub frames: u64,
    /// Длительность по фактическому звуку, мс.
    pub duration_ms: u64,
    /// Сколько кадров потеряла страница до привязки биндинга.
    pub dropped_by_page: u64,
    /// Пути созданных файлов.
    pub files: Vec<PathBuf>,
}

/// Приёмник звука и событий одной встречи.
pub struct Recorder {
    meeting_dir: PathBuf,
    sample_rate: u32,
    tracks: TrackRegistry,
    writers: HashMap<usize, WavWriter>,
    journal: EventJournal,
    manifest: Manifest,
    stats: TransportStats,
    /// Наибольшее время, увиденное на оси. Нужно, чтобы закрыть дорожки
    /// концом ЗАПИСИ, а не нулём, если события `track_ended` не пришло.
    max_ms: u64,
}

impl Recorder {
    /// Открывает каталог встречи и готовит запись.
    pub fn create(
        meeting_dir: impl Into<PathBuf>,
        manifest: Manifest,
    ) -> anyhow::Result<Self> {
        let meeting_dir = meeting_dir.into();
        std::fs::create_dir_all(meeting_dir.join(RAW_DIR))?;
        let journal = EventJournal::open(&meeting_dir)?;
        let sample_rate = manifest.sample_rate;
        manifest.save(&meeting_dir)?;
        Ok(Self {
            meeting_dir,
            sample_rate,
            tracks: TrackRegistry::new(),
            writers: HashMap::new(),
            journal,
            manifest,
            stats: TransportStats::default(),
            max_ms: 0,
        })
    }

    pub fn meeting_dir(&self) -> &Path {
        &self.meeting_dir
    }

    pub fn tracks(&self) -> &TrackRegistry {
        &self.tracks
    }

    pub fn stats(&self) -> TransportStats {
        self.stats
    }

    /// Путь файла дорожки.
    pub fn track_path(&self, idx: usize) -> PathBuf {
        self.meeting_dir.join(RAW_DIR).join(format!("track-{idx}.wav"))
    }

    /// Принимает одно сообщение со страницы.
    ///
    /// Ошибка записи ОДНОГО кадра не прерывает встречу: диск мог моргнуть, а
    /// потерянный кадр по устройству формата означает тишину на своём месте.
    pub fn accept(&mut self, msg: PageMessage) -> anyhow::Result<()> {
        match msg {
            PageMessage::Audio(frame) => self.accept_frame(frame),
            PageMessage::Event(ev) => self.accept_event(ev),
            PageMessage::Meta { dropped, buffered } => {
                self.stats.dropped_by_page = dropped;
                if dropped > 0 {
                    let _ = self.journal.note(
                        self.max_ms,
                        "frames_dropped",
                        json!({"dropped": dropped, "buffered": buffered}),
                    );
                }
                Ok(())
            }
        }
    }

    fn accept_frame(&mut self, frame: AudioFrame) -> anyhow::Result<()> {
        // Кадр от НЕИЗВЕСТНОЙ дорожки — не ошибка: сообщение о её появлении
        // могло отстать (события и звук идут разными каналами). Заводим
        // дорожку по факту звука, а смещением берём начало кадра.
        let idx = match self.tracks.get(&frame.track_id) {
            Some(t) => t.idx,
            None => {
                let idx = self.tracks.open(&frame.track_id, frame.start_ms()).idx;
                let _ = self.journal.note(
                    frame.start_ms(),
                    "track_added_implicitly",
                    json!({"t": frame.track_id, "idx": idx}),
                );
                idx
            }
        };
        self.tracks
            .note_frame(&frame.track_id, frame.offset_samples, frame.len());

        let rate = self.sample_rate;
        let path = self.track_path(idx);
        let writer = match self.writers.entry(idx) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(WavWriter::create(&path, rate)?)
            }
        };
        writer.write_frame(frame.offset_samples, &frame.pcm)?;

        self.stats.frames += 1;
        self.stats.samples += frame.len() as u64;
        self.max_ms = self.max_ms.max(frame.end_ms());
        Ok(())
    }

    fn accept_event(&mut self, ev: PageEvent) -> anyhow::Result<()> {
        self.max_ms = self.max_ms.max(ev.at_ms);
        let _ = self
            .journal
            .note(ev.at_ms, &ev.name, ev.fields.clone());

        match ev.name.as_str() {
            "track_added" => {
                if let Some(id) = ev.str_field("t") {
                    let start = ev.u64_field("off").unwrap_or(ev.at_ms);
                    let id = id.to_string();
                    self.tracks.open(&id, start);
                    if let Some(path) = ev.str_field("p") {
                        self.tracks.note_path(&id, path);
                    }
                }
            }
            "track_ended" => {
                if let Some(id) = ev.str_field("t") {
                    let id = id.to_string();
                    self.tracks.close(&id, ev.at_ms);
                }
            }
            "capture_path" => {
                if let Some(p) = ev.str_field("p") {
                    self.manifest.capture_path = Some(p.to_string());
                }
            }
            "recording_started" => {
                if let Some(unix) = ev.u64_field("unix") {
                    self.manifest.t0_unix_ms = unix as i64;
                }
            }
            "track_mode" => {
                if let Some(m) = ev.str_field("mode").and_then(TrackMode::parse) {
                    self.manifest.track_mode = Some(m);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Обновляет режим SFU, определённый снаружи (`detect_track_mode`).
    pub fn set_track_mode(&mut self, mode: TrackMode) {
        self.manifest.track_mode = Some(mode);
    }

    /// Принимает ТИПИЗИРОВАННОЕ событие бота.
    ///
    /// Отдельный вход рядом с [`Recorder::accept`] потому, что источников
    /// событий два и они разной природы: сырые сообщения страницы приходят
    /// через транспорт, а [`crate::hub::MeetEvent`] — от самого бота (живого
    /// или сценарного). Сводить их в один тип означало бы гонять типизованное
    /// событие через JSON и обратно ради единообразия.
    pub fn note_meet_event(&mut self, ev: &crate::hub::MeetEvent) {
        use crate::hub::MeetEvent as E;
        let at = ev.at_ms().unwrap_or(self.max_ms);
        self.max_ms = self.max_ms.max(at);
        let data = serde_json::to_value(ev).unwrap_or(serde_json::Value::Null);
        let _ = self.journal.note(at, ev.kind(), data);

        match ev {
            E::RecordingStarted { t0_unix_ms } => self.manifest.t0_unix_ms = *t0_unix_ms,
            E::CapturePathDetected { path } => self.manifest.capture_path = Some(path.clone()),
            E::TrackModeDetected { mode, .. } => self.manifest.track_mode = Some(*mode),
            E::TrackAdded { track_id, start_offset_ms, .. } => {
                self.tracks.open(track_id, *start_offset_ms);
            }
            E::TrackEnded { track_id, end_offset_ms, .. } => {
                self.tracks.close(track_id, *end_offset_ms);
            }
            E::FramesDropped { total } => self.stats.dropped_by_page = *total,
            _ => {}
        }
    }

    /// Периодический снимок манифеста.
    ///
    /// Зовётся по таймеру, а не на каждом кадре: манифест перезаписывается
    /// целиком, и делать это пятьдесят раз в секунду было бы расточительством
    /// без единой выгоды.
    pub fn checkpoint(&mut self) -> anyhow::Result<()> {
        self.sync_manifest();
        self.manifest.save(&self.meeting_dir)
    }

    fn sync_manifest(&mut self) {
        self.manifest.tracks = self.tracks.all().to_vec();
        self.manifest.transport = self.stats;
        self.manifest.duration_ms = self.max_ms;
    }

    /// Закрывает запись: дофлашивает WAV, чинит заголовки, пишет манифест.
    ///
    /// Именно здесь заголовки получают правильную длину. Пропусти мы этот шаг
    /// — файлы всё равно останутся читаемыми (`repair_header` восстановит
    /// длину по размеру), но плеер до починки показывал бы нулевую
    /// длительность.
    pub fn finish(mut self) -> anyhow::Result<RecorderReport> {
        let end = self.max_ms;
        self.tracks.close_all(end);

        let mut files = Vec::new();
        let writers = std::mem::take(&mut self.writers);
        let mut ordered: Vec<(usize, WavWriter)> = writers.into_iter().collect();
        ordered.sort_by_key(|(idx, _)| *idx);
        for (idx, writer) in ordered {
            let path = writer.path().to_path_buf();
            writer.finish()?;
            files.push(path);
            let _ = self.journal.note(end, "track_closed", json!({"idx": idx}));
        }

        self.sync_manifest();
        self.manifest.finished = true;
        self.manifest.save(&self.meeting_dir)?;
        let _ = self.journal.note(end, "recording_finished", json!({"duration_ms": end}));

        Ok(RecorderReport {
            tracks: self.tracks.total(),
            frames: self.stats.frames,
            duration_ms: end,
            dropped_by_page: self.stats.dropped_by_page,
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::transport::parse_page_message;
    use crate::ingest::wav::read_samples;

    fn manifest() -> Manifest {
        Manifest::new("m1", "https://telemost.yandex.ru/j/1", "telemost", "normal", 16_000)
    }

    fn frame(track: &str, offset: u64, value: i16, n: usize) -> PageMessage {
        PageMessage::Audio(AudioFrame::new(track, offset, 16_000, vec![value; n]))
    }

    fn event(name: &str, at_ms: u64, fields: serde_json::Value) -> PageMessage {
        let mut v = fields;
        v["k"] = json!("e");
        v["n"] = json!(name);
        v["ms"] = json!(at_ms);
        parse_page_message(&v.to_string(), 16_000).unwrap()
    }

    /// Полный проход: две дорожки, перекрывающаяся речь, файлы на диске.
    #[test]
    fn a_full_recording_lands_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), manifest()).unwrap();

        r.accept(event("recording_started", 0, json!({"unix": 1_700_000_000_000u64}))).unwrap();
        r.accept(event("capture_path", 0, json!({"p": "rtcpeerconnection"}))).unwrap();
        r.accept(event("track_added", 0, json!({"t": "a", "idx": 0, "off": 0}))).unwrap();
        r.accept(event("track_added", 0, json!({"t": "b", "idx": 1, "off": 0}))).unwrap();

        for i in 0..50u64 {
            r.accept(frame("a", i * 320, 1000, 320)).unwrap();
            r.accept(frame("b", i * 320, 2000, 320)).unwrap();
        }
        r.accept(event("track_ended", 1_000, json!({"t": "a"}))).unwrap();
        r.accept(event("track_ended", 1_000, json!({"t": "b"}))).unwrap();

        let report = r.finish().unwrap();
        assert_eq!(report.tracks, 2);
        assert_eq!(report.frames, 100);
        assert_eq!(report.duration_ms, 1_000);
        assert_eq!(report.files.len(), 2);

        for (idx, value) in [(0usize, 1000i16), (1, 2000)] {
            let path = dir.path().join(RAW_DIR).join(format!("track-{idx}.wav"));
            assert!(path.is_file(), "нет файла {}", path.display());
            let samples = read_samples(&path).unwrap();
            assert_eq!(samples.len(), 50 * 320);
            assert!(samples.iter().all(|&s| s == value));
        }

        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.finished, "манифест обязан быть закрыт");
        assert_eq!(m.capture_path.as_deref(), Some("rtcpeerconnection"));
        assert_eq!(m.t0_unix_ms, 1_700_000_000_000);
        assert_eq!(m.tracks.len(), 2);
        assert_eq!(m.duration_ms, 1_000);

        let journal = EventJournal::read(dir.path()).unwrap();
        assert!(journal.iter().any(|e| e.kind == "recording_started"));
        assert!(journal.iter().any(|e| e.kind == "recording_finished"));
    }

    /// Кадры В ПЕРЕМЕШАННОМ ПОРЯДКЕ дают файл, идентичный поданным по порядку.
    #[test]
    fn shuffled_frames_produce_an_identical_file() {
        let ordered = tempfile::tempdir().unwrap();
        let shuffled = tempfile::tempdir().unwrap();

        let frames: Vec<PageMessage> = (0..20)
            .map(|i| frame("a", i * 320, (i as i16 + 1) * 10, 320))
            .collect();

        let mut r = Recorder::create(ordered.path(), manifest()).unwrap();
        for f in frames.clone() {
            r.accept(f).unwrap();
        }
        r.finish().unwrap();

        let mut r = Recorder::create(shuffled.path(), manifest()).unwrap();
        let mut order: Vec<usize> = (0..frames.len()).rev().collect();
        order.swap(2, 9);
        for i in order {
            r.accept(frames[i].clone()).unwrap();
        }
        r.finish().unwrap();

        let a = std::fs::read(ordered.path().join(RAW_DIR).join("track-0.wav")).unwrap();
        let b = std::fs::read(shuffled.path().join(RAW_DIR).join("track-0.wav")).unwrap();
        assert_eq!(a, b, "порядок прихода кадров изменил файл");
    }

    /// ПРОПУСК КАДРА оставляет ровно свою тишину и не сдвигает остальное.
    #[test]
    fn a_missing_frame_leaves_silence_and_keeps_the_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), manifest()).unwrap();
        for i in 0..10u64 {
            if i == 4 {
                continue; // кадр «потерян»
            }
            r.accept(frame("a", i * 320, 500, 320)).unwrap();
        }
        r.finish().unwrap();

        let samples = read_samples(&dir.path().join(RAW_DIR).join("track-0.wav")).unwrap();
        assert_eq!(samples.len(), 10 * 320, "дырка обязана остаться в файле");
        let gap = &samples[4 * 320..5 * 320];
        assert!(gap.iter().all(|&s| s == 0), "на месте кадра нет тишины");
        let after = &samples[5 * 320..];
        assert!(after.iter().all(|&s| s == 500), "сэмплы после дырки сдвинулись");
    }

    /// Ушедший участник не отдаёт номер файла пришедшему.
    #[test]
    fn a_new_participant_gets_a_new_file_not_the_freed_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), manifest()).unwrap();

        r.accept(event("track_added", 0, json!({"t": "a", "off": 0}))).unwrap();
        r.accept(frame("a", 0, 111, 320)).unwrap();
        r.accept(event("track_ended", 2_000, json!({"t": "a"}))).unwrap();

        r.accept(event("track_added", 2_500, json!({"t": "b", "off": 2500}))).unwrap();
        r.accept(frame("b", 16_000 * 2_500 / 1_000, 222, 320)).unwrap();

        let report = r.finish().unwrap();
        assert_eq!(report.tracks, 2);
        assert!(dir.path().join(RAW_DIR).join("track-0.wav").is_file());
        assert!(
            dir.path().join(RAW_DIR).join("track-1.wav").is_file(),
            "вторая дорожка обязана получить свой файл"
        );

        // И вторая дорожка начинается СО СМЕЩЕНИЕМ на общей оси.
        let late = read_samples(&dir.path().join(RAW_DIR).join("track-1.wav")).unwrap();
        assert_eq!(late.len(), 16_000 * 2_500 / 1_000 + 320);
        assert!(late[..100].iter().all(|&s| s == 0), "файл обязан начинаться тишиной");
        assert!(late[late.len() - 100..].iter().all(|&s| s == 222));

        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.tracks[1].start_offset_ms, 2_500);
        assert_eq!(m.tracks[0].end_offset_ms, Some(2_000));
    }

    /// Кадр пришёл РАНЬШЕ события о дорожке: события и звук идут разными
    /// каналами, и порядок между ними не гарантирован.
    #[test]
    fn audio_arriving_before_the_track_event_still_gets_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), manifest()).unwrap();
        r.accept(frame("a", 0, 777, 320)).unwrap();
        r.accept(event("track_added", 0, json!({"t": "a", "off": 0}))).unwrap();
        r.accept(frame("a", 320, 777, 320)).unwrap();

        let report = r.finish().unwrap();
        assert_eq!(report.tracks, 1, "дорожка не должна задвоиться");
        assert_eq!(report.frames, 2);
        let samples = read_samples(&dir.path().join(RAW_DIR).join("track-0.wav")).unwrap();
        assert_eq!(samples.len(), 640);
    }

    /// ОБРЫВ: без `finish` файлы остаются на диске и чинятся по размеру.
    /// Ровно на этом стоит спасение записи после Ctrl-C.
    #[test]
    fn an_interrupted_recording_leaves_salvageable_audio() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut r = Recorder::create(dir.path(), manifest()).unwrap();
            for i in 0..100u64 {
                r.accept(frame("a", i * 320, 42, 320)).unwrap();
            }
            r.checkpoint().unwrap();
            // `r` уходит без finish — так выглядит Ctrl-C по devforge.
        }

        let path = dir.path().join(RAW_DIR).join("track-0.wav");
        assert!(path.is_file());
        let samples = crate::ingest::wav::repair_header(&path, 16_000).unwrap();
        assert_eq!(samples, 100 * 320, "спасаемого звука ровно две секунды");

        let m = Manifest::load(dir.path()).unwrap();
        assert!(!m.finished, "манифест обязан помнить, что запись не закрыта");
        assert_eq!(m.tracks.len(), 1);
    }

    /// Счётчик потерь страницы доезжает до манифеста: молчаливая потеря
    /// первых секунд записи — худший исход, потому что она незаметна.
    #[test]
    fn page_side_losses_reach_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), manifest()).unwrap();
        r.accept(PageMessage::Meta { dropped: 12, buffered: 3 }).unwrap();
        r.accept(frame("a", 0, 1, 320)).unwrap();
        let report = r.finish().unwrap();

        assert_eq!(report.dropped_by_page, 12);
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.transport.dropped_by_page, 12);
        let journal = EventJournal::read(dir.path()).unwrap();
        assert!(journal.iter().any(|e| e.kind == "frames_dropped"));
    }

    /// ГЕРМЕТИЧНОСТЬ: рекордер пишет ТОЛЬКО в отданный ему каталог.
    #[test]
    fn nothing_is_written_outside_the_meeting_directory() {
        let root = tempfile::tempdir().unwrap();
        let meeting = root.path().join("meetings/m1");
        let mut r = Recorder::create(&meeting, manifest()).unwrap();
        r.accept(frame("a", 0, 1, 320)).unwrap();
        let report = r.finish().unwrap();

        for f in &report.files {
            assert!(f.starts_with(&meeting), "файл вне каталога встречи: {}", f.display());
        }
        // В корне только каталог встречи и ничего больше.
        let entries: Vec<String> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["meetings"]);
    }
}
