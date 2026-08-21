//! `manifest.json` — паспорт записи.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Зачем он нужен, если есть база
//!
//! База знает статус встречи, манифест — устройство записи: какой тап
//! сработал, какой режим SFU определился, где `T0`, сколько было дорожек и с
//! какими смещениями. Всё это нужно ПОСТ-ОБРАБОТКЕ, которая может запуститься
//! после перезапуска devforge — и обязана понять каталог, ничего не помня.
//!
//! Каталог записи самодостаточен намеренно: `~/.forge/meet/<id>/` можно
//! скопировать на другую машину и доработать там.
//!
//! # Пишется АТОМАРНО
//!
//! Через [`forge_atomic`], а не `File::create` + `write`. Манифест
//! перезаписывается на каждом изменении состава дорожек, и прерывание посреди
//! записи оставило бы обрезанный JSON — то есть каталог, который
//! пост-обработка не сможет прочитать вовсе. Четвёртую копию
//! `tmp + rename + chmod` не заводим: она для того и вынесена в общий крейт.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::tracks::TrackState;
use crate::bot::mode::TrackMode;
use crate::bot::transport::TransportStats;

/// Имя файла в каталоге встречи.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Версия формата. Растёт при несовместимых изменениях: каталог с чужой
/// версией лучше не трогать, чем прочитать наполовину.
pub const MANIFEST_VERSION: u32 = 1;

/// Паспорт записи.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub meeting_id: String,
    /// Канонический URL комнаты.
    pub url: String,
    pub platform: String,
    /// `normal` либо `assisted`.
    pub mode: String,
    /// Момент `T0` в unix-миллисекундах — ТОЛЬКО чтобы человек мог соотнести
    /// запись с календарём. В арифметике таймлайна не участвует НИКОГДА:
    /// подмешай мы сюда время бэкенда, в ось просочился бы джиттер CDP.
    pub t0_unix_ms: i64,
    pub sample_rate: u32,
    /// Какой из трёх тапов реально дал звук.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_path: Option<String>,
    /// Режим SFU, определённый в рантайме.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_mode: Option<TrackMode>,
    /// Дорожки в порядке появления, включая закрытые.
    pub tracks: Vec<TrackState>,
    /// Счётчики транспорта. Потери обязаны быть видны.
    pub transport: TransportStats,
    /// Длительность записи по фактическому звуку, мс.
    pub duration_ms: u64,
    /// Закрыта ли запись штатно. `false` в файле означает, что процесс
    /// прервали — и это же читает `reconcile_on_boot`.
    pub finished: bool,
    /// Что сделала пост-обработка. `None` — она ещё не доходила до каталога.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postprocess: Option<PostprocessInfo>,
    /// Как получились имена спикеров (Ф4). `None` — атрибуция ещё не шла.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speakers: Option<SpeakerBinding>,
}

/// След каскада привязки имён.
///
/// Живёт в манифесте, потому что БЕЗ НЕГО НЕВОЗМОЖНО ПОНЯТЬ, ДОВЕРЯТЬ ЛИ
/// ИМЕНАМ. «Алиса» из `msid` — это факт, «Алиса» из корреляции с уверенностью
/// 0,71 — это гипотеза, и человеку, читающему расшифровку через месяц, разница
/// принципиальна. Каталог встречи самодостаточен, поэтому след пишется рядом с
/// записью, а не только в базу.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerBinding {
    /// Лучшая сработавшая ступень каскада:
    /// `msid` | `ssrc` | `correlation` | `dom` | `fallback`.
    pub stage: String,
    /// Откуда взяты интервалы речи: `vad` (пер-трековый детект) либо `dom`
    /// (индикатор говорящего площадки).
    pub source: String,
    /// Сколько дорожек получили настоящее имя.
    pub named_tracks: usize,
    /// Сколько дорожек всего.
    pub tracks: usize,
    /// Наибольшая уверенность среди названных.
    pub confidence: f64,
}

/// След пост-обработки в паспорте записи.
///
/// Живёт в манифесте, а не только в базе, потому что каталог встречи
/// самодостаточен: скопировав `~/.forge/meet/<id>/` на другую машину, надо
/// уметь ответить, почему таймкоды такие, — не имея базы.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostprocessInfo {
    /// Проверялась ли длительность аудио по `ffprobe`.
    ///
    /// `false` означает «ffprobe не было» — это НЕ ошибка, но факт пропуска
    /// обязан быть виден: иначе через месяц не понять, почему таймкоды
    /// слегка плывут относительно журнала событий.
    pub drift_checked: bool,
    /// Применённый множитель линейной коррекции. `None` — расхождение было
    /// меньше порога.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_scale: Option<f64>,
    /// Фактическая длительность сведённого аудио по `ffprobe`, мс.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_duration_ms: Option<u64>,
    /// Собран ли архивный `audio.m4a`.
    pub archive: bool,
    /// Пик сведённого аудио по `volumedetect`, dBFS. `None` — ffmpeg не было
    /// или он не напечатал статистику.
    ///
    /// Хранится ради разбора задним числом: цифра сразу отвечает на вопрос
    /// «звук вообще записался?», ради которого иначе нужен ffmpeg и час
    /// времени. У встречи, где медиа-канал не поднялся, здесь -91,0 — пол
    /// 16-битной сетки.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix_peak_db: Option<f64>,
    /// Сколько дорожек расшифровано ПО ОТДЕЛЬНОСТИ (режим `PerParticipant` с
    /// нарезкой по VAD). `0` — расшифровывали сведённый микс.
    ///
    /// Живёт здесь по той же причине, что и след каскада: по таймкодам
    /// невозможно понять, чем они получены, а разница принципиальна. При
    /// пер-трековой расшифровке подпись спикера — факт (дорожка = человек),
    /// при миксе — результат перекрытия с интервалами речи.
    #[serde(default)]
    pub per_track: usize,
}

impl Manifest {
    /// Пустой манифест только что начатой записи.
    pub fn new(meeting_id: &str, url: &str, platform: &str, mode: &str, sample_rate: u32) -> Self {
        Self {
            version: MANIFEST_VERSION,
            meeting_id: meeting_id.to_string(),
            url: url.to_string(),
            platform: platform.to_string(),
            mode: mode.to_string(),
            t0_unix_ms: 0,
            sample_rate,
            capture_path: None,
            track_mode: None,
            tracks: Vec::new(),
            transport: TransportStats::default(),
            duration_ms: 0,
            finished: false,
            postprocess: None,
            speakers: None,
        }
    }

    /// Путь манифеста в каталоге встречи.
    pub fn path_in(meeting_dir: &Path) -> std::path::PathBuf {
        meeting_dir.join(MANIFEST_FILE)
    }

    /// Пишет манифест АТОМАРНО.
    pub fn save(&self, meeting_dir: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        forge_atomic::write_bytes_atomic(&Self::path_in(meeting_dir), &json)
    }

    /// Читает манифест. `None` — файла нет либо он не разбирается.
    ///
    /// Битый манифест НЕ повод падать: каталог со звуком всё равно ценнее
    /// файла с метаданными, и `reconcile_on_boot` умеет обойтись без него.
    pub fn load(meeting_dir: &Path) -> Option<Self> {
        let raw = std::fs::read(Self::path_in(meeting_dir)).ok()?;
        serde_json::from_slice(&raw).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(idx: usize, start: u64) -> TrackState {
        TrackState {
            track_id: format!("t{idx}"),
            idx,
            start_offset_ms: start,
            end_offset_ms: None,
            frames: 10,
            samples: 3_200,
            end_samples: 3_200,
            capture_path: None,
        }
    }

    #[test]
    fn the_manifest_survives_a_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new("m1", "https://telemost.yandex.ru/j/1", "telemost", "normal", 16_000);
        m.t0_unix_ms = 1_700_000_000_000;
        m.capture_path = Some("rtcpeerconnection".into());
        m.track_mode = Some(TrackMode::PerParticipant);
        m.tracks = vec![track(0, 0), track(1, 2_500)];
        m.duration_ms = 8_000;

        m.save(dir.path()).unwrap();
        let back = Manifest::load(dir.path()).expect("манифест не прочитался");
        assert_eq!(back, m);
        assert_eq!(back.tracks[1].start_offset_ms, 2_500);
        assert_eq!(back.track_mode, Some(TrackMode::PerParticipant));
    }

    /// Манифест обязан нести ровно то, что нужно пост-обработке, ничего не
    /// зная про базу: каталог самодостаточен.
    #[test]
    fn the_manifest_carries_everything_post_processing_needs() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new("m1", "u", "telemost", "normal", 16_000);
        m.capture_path = Some("webaudio".into());
        m.tracks = vec![track(0, 0)];
        m.save(dir.path()).unwrap();

        let raw = std::fs::read_to_string(Manifest::path_in(dir.path())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], MANIFEST_VERSION);
        assert_eq!(v["sample_rate"], 16_000);
        assert_eq!(v["capture_path"], "webaudio");
        assert_eq!(v["tracks"][0]["idx"], 0);
        assert_eq!(v["finished"], false, "незакрытая запись обязана быть видна");
        // Пустые поля не сериализуются: манифест читают глазами.
        assert!(!raw.contains("\"track_mode\""));
    }

    /// Битый манифест не роняет разбор каталога: звук ценнее метаданных.
    #[test]
    fn a_broken_manifest_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Manifest::path_in(dir.path()), "{ это не json").unwrap();
        assert!(Manifest::load(dir.path()).is_none());

        let empty = tempfile::tempdir().unwrap();
        assert!(Manifest::load(empty.path()).is_none());
    }

    /// АТОМАРНОСТЬ: старый манифест не исчезает, пока новый не записан
    /// целиком. Проверяется через `forge_atomic` — второй записи в тот же путь
    /// не бывает.
    #[test]
    fn saving_twice_never_leaves_a_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new("m1", "u", "telemost", "normal", 16_000);
        m.save(dir.path()).unwrap();

        for n in 1..=20 {
            m.tracks = (0..n).map(|i| track(i, i as u64 * 100)).collect();
            m.save(dir.path()).unwrap();
            // После КАЖДОЙ записи файл обязан разбираться целиком.
            let back = Manifest::load(dir.path()).expect("манифест разъехался на записи");
            assert_eq!(back.tracks.len(), n);
        }
    }
}
