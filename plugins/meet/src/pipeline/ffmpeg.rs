//! Сведение пер-трековых WAV и архивный аудиофайл встречи.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Цепочка фильтров на каждую дорожку
//!
//! | фильтр | зачем именно он |
//! |---|---|
//! | `aresample=async=1:first_pts=0` | ЗАПОЛНЯЕТ ТИШИНОЙ дыры от потерянных пакетов. Без него таймлайн уезжает на длину каждого пропуска, и привязка спикеров разъезжается вместе с ним |
//! | `adelay=<мс>:all=1` | ставит дорожку на общую ось (см. [`TrackOrigin`]) |
//! | `highpass=f=80` | срез низкочастотного гула |
//! | `speechnorm` | выравнивание громкости, спроектированное под речь: однопроходное и, в отличие от однопроходного `loudnorm`, не «качает» уровень |
//!
//! Микс — `amix=inputs=N:duration=longest:normalize=0`.
//!
//! # `normalize=0` ОБЯЗАТЕЛЕН
//!
//! Дефолт `normalize=1` делит громкость на число входов: при пяти дорожках
//! всё становится в пять раз тише. Ошибка тихая — файл получается, просто
//! хуже распознаётся, и заметить это можно только по качеству транскрипта
//! через полчаса. Поэтому на строку есть ОТДЕЛЬНЫЙ регресс-тест
//! ([`tests::normalize_zero_is_present_for_any_number_of_inputs`]), а не
//! только golden.
//!
//! # Архив — AAC, а вход whisper — WAV
//!
//! Это два РАЗНЫХ файла, и путать их нельзя. Архив (`audio.m4a`, AAC 32
//! кбит/с моно, `+faststart`) весит 14 МБ на час, и на такой величине
//! `fetch` + `blob:` во фронте даёт идеальную перемотку. Whisper.cpp его
//! прочитать не может: поддерживаются `flac`, `mp3`, `ogg`, `wav` (измерено в
//! Ф0). Поэтому расшифровка получает `work/mix.wav`.
//!
//! # Без ffmpeg транскрипт ВСЁ РАВНО ЕСТЬ
//!
//! По проводу от бота идёт готовый PCM16 16 кГц моно, поэтому свести дорожки
//! умеет и [`mix_tracks_in_memory`] — двадцать строк на сложение с
//! ограничением. Без ffmpeg пропадает только архивный файл и фильтры
//! обработки; расшифровка остаётся. Ровно это и означает «ffmpeg убран с
//! критического пути».

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::proc::{self, ProcError};
use crate::ingest::tracks::TrackState;

/// Дорожка, входящая в микс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixTrack {
    /// Файл `raw/track-N.wav`.
    pub path: PathBuf,
    /// Задержка перед постановкой на общую ось, мс.
    pub delay_ms: u64,
}

/// Откуда считается ноль пер-трекового файла.
///
/// **Разница не теоретическая, и ошибка в ней сдвигает всю атрибуцию.**
///
/// Ingest Ф3 адресует кадры АБСОЛЮТНЫМ сэмпловым смещением от `T0`, поэтому
/// дорожка, появившаяся на пятнадцатой минуте, начинается в файле пятнадцатью
/// минутами тишины — файл УЖЕ на общей оси. Добавь мы такой дорожке
/// `adelay=900000`, она уехала бы на полчаса.
///
/// План писался в расчёте на второй вариант ([`TrackOrigin::OwnZero`]), где
/// файл начинается с первого сэмпла дорожки; он остаётся здесь потому, что
/// это ровно то, что отдаёт `MediaRecorder`, и Ф8–Ф10 могут им
/// воспользоваться. Какой из двух — решает [`mix_tracks`], и это единственное
/// место, которое придётся поменять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrigin {
    /// Файл начинается в момент `T0` встречи: пропуски и поздний старт уже
    /// заполнены тишиной. Так пишет `ingest::Recorder`.
    SharedT0,
    /// Файл начинается с первого сэмпла дорожки.
    OwnZero,
}

/// Собирает список дорожек микса из состояния записи.
///
/// `delay_ms` берётся из `start_offset_ms` дорожки — но только для
/// [`TrackOrigin::OwnZero`]. Разбор — в доке [`TrackOrigin`].
pub fn mix_tracks(tracks: &[TrackState], raw_dir: &Path, origin: TrackOrigin) -> Vec<MixTrack> {
    let mut out: Vec<MixTrack> = tracks
        .iter()
        .map(|t| MixTrack {
            path: raw_dir.join(t.file_name()),
            delay_ms: match origin {
                TrackOrigin::SharedT0 => 0,
                TrackOrigin::OwnZero => t.start_offset_ms,
            },
        })
        .filter(|m| m.path.is_file())
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Собирает фильтрограф ffmpeg. **Чистая функция.**
///
/// Пустой список даёт пустую строку: сводить нечего, и вызывающий обязан
/// заметить это раньше, чем запустит ffmpeg.
pub fn build_mixdown_filtergraph(tracks: &[MixTrack]) -> String {
    if tracks.is_empty() {
        return String::new();
    }
    let mut chains: Vec<String> = Vec::with_capacity(tracks.len() + 1);
    for (i, t) in tracks.iter().enumerate() {
        chains.push(format!(
            "[{i}:a]aresample=async=1:first_pts=0,adelay={}:all=1,highpass=f=80,speechnorm[a{i}]",
            t.delay_ms
        ));
    }
    let labels: String = (0..tracks.len()).map(|i| format!("[a{i}]")).collect();
    chains.push(format!(
        "{labels}amix=inputs={}:duration=longest:normalize=0[mix]",
        tracks.len()
    ));
    chains.join(";")
}

/// argv сведения дорожек в один WAV 16 кГц моно.
pub fn build_mixdown_args(tracks: &[MixTrack], out: &Path, sample_rate: u32) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        // Перезапись без вопросов: этап идемпотентен по детерминированному
        // пути, и `-y` — единственный способ не встать на вопросе «файл
        // существует, перезаписать?» в фоновом процессе.
        "-y".into(),
    ];
    for t in tracks {
        args.push("-i".into());
        args.push(t.path.display().to_string());
    }
    args.push("-filter_complex".into());
    args.push(build_mixdown_filtergraph(tracks));
    args.push("-map".into());
    args.push("[mix]".into());
    args.push("-c:a".into());
    args.push("pcm_s16le".into());
    args.push("-ar".into());
    args.push(sample_rate.to_string());
    args.push("-ac".into());
    args.push("1".into());
    args.push(out.display().to_string());
    args
}

/// argv архивного файла: AAC 32 кбит/с моно с `+faststart`.
pub fn build_archive_args(input: &Path, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-i".into(),
        input.display().to_string(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "32k".into(),
        "-ac".into(),
        "1".into(),
        // Индекс в начало файла: без этого плеер обязан скачать весь файл,
        // прежде чем показать длительность, — а фронт грузит аудио одним
        // `fetch` в blob именно ради перемотки.
        "-movflags".into(),
        "+faststart".into(),
        out.display().to_string(),
    ]
}

/// argv замера громкости: прогон через `volumedetect` без записи файла.
///
/// Выход направлен в `null`, то есть звук никуда не пишется — фильтр только
/// считает статистику и печатает её в stderr. Час записи меряется за доли
/// секунды (чтение PCM без кодирования), поэтому гейт тишины не стоит
/// пользователю ничего.
pub fn build_loudness_args(input: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-nostats".into(),
        "-i".into(),
        input.display().to_string(),
        "-af".into(),
        "volumedetect".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ]
}

/// Громкость сведённого файла: среднее и пик, dBFS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Среднеквадратичная громкость всей записи, dBFS.
    pub mean_db: f64,
    /// Пиковая громкость, dBFS. Именно её сравнивает гейт тишины: встреча с
    /// единственной фразой за час имеет низкое среднее при высоком пике.
    pub max_db: f64,
}

/// Мерит громкость файла через `volumedetect`.
///
/// `None` — фильтр не напечатал статистику (файл без сэмплов, неизвестный
/// формат). Это НЕ ошибка этапа: гейт тишины в таком случае просто не
/// срабатывает, а факт уезжает в журнал встречи.
pub async fn measure_loudness(
    ffmpeg: &Path,
    input: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Option<Loudness>, ProcError> {
    // `proc::run` отдаёт ХВОСТ stderr, а `volumedetect` печатает свою сводку
    // последней — то есть искомые строки в хвосте всегда.
    let stderr = proc::run(ffmpeg, &build_loudness_args(input), timeout, cancel).await?;
    Ok(parse_loudness(&stderr))
}

/// Разбирает сводку `volumedetect` из stderr ffmpeg.
///
/// Берётся ПОСЛЕДНЯЯ пара значений: у фильтра бывает несколько экземпляров
/// (первый прогон печатает `n_samples: 0`), и итог — тот, что напечатан
/// последним.
pub fn parse_loudness(stderr: &str) -> Option<Loudness> {
    let mut mean = None;
    let mut max = None;
    for line in stderr.lines() {
        if let Some(v) = parse_db_after(line, "mean_volume:") {
            mean = Some(v);
        }
        if let Some(v) = parse_db_after(line, "max_volume:") {
            max = Some(v);
        }
    }
    Some(Loudness {
        mean_db: mean?,
        max_db: max?,
    })
}

/// Достаёт число из строки вида `[Parsed_volumedetect_0 @ …] max_volume: -91.0 dB`.
fn parse_db_after(line: &str, key: &str) -> Option<f64> {
    let rest = line.split_once(key)?.1.trim();
    let value = rest.strip_suffix("dB").unwrap_or(rest).trim();
    value.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// argv `ffprobe` для длительности файла в секундах.
pub fn build_probe_args(path: &Path) -> Vec<String> {
    vec![
        "-v".into(),
        "error".into(),
        "-show_entries".into(),
        "format=duration".into(),
        "-of".into(),
        "default=noprint_wrappers=1:nokey=1".into(),
        path.display().to_string(),
    ]
}

/// Сводит дорожки через ffmpeg.
pub async fn mixdown_with_ffmpeg(
    ffmpeg: &Path,
    tracks: &[MixTrack],
    out: &Path,
    sample_rate: u32,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProcError> {
    if tracks.is_empty() {
        return Err(ProcError::new("no_tracks", "нечего сводить: дорожек нет", ""));
    }
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let args = build_mixdown_args(tracks, out, sample_rate);
    proc::run(ffmpeg, &args, timeout, cancel).await.map(|_| ())
}

/// Собирает архивный AAC из уже сведённого WAV.
pub async fn encode_archive(
    ffmpeg: &Path,
    input: &Path,
    out: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), ProcError> {
    let args = build_archive_args(input, out);
    proc::run(ffmpeg, &args, timeout, cancel).await.map(|_| ())
}

/// Длительность файла по `ffprobe`, мс. `None` — ffprobe нет или он не понял
/// файл.
pub async fn probe_duration_ms(
    ffprobe: &Path,
    path: &Path,
    timeout: Duration,
) -> Option<u64> {
    let out = proc::capture_stdout(ffprobe, &build_probe_args(path), timeout)
        .await
        .ok()?;
    parse_duration_secs(&out).map(|s| (s * 1000.0).round() as u64)
}

/// Разбирает вывод `ffprobe -show_entries format=duration`.
pub fn parse_duration_secs(raw: &str) -> Option<f64> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    // `N/A` ffprobe печатает у потоков без длительности — это не число и не
    // ошибка разбора, а честное «не знаю».
    line.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// Сводит дорожки СВОИМИ СИЛАМИ, без ffmpeg.
///
/// Существует ради инварианта «ffmpeg не на критическом пути расшифровки»: по
/// проводу уже идёт PCM16 16 кГц моно, и сложить несколько таких дорожек —
/// это сложение с ограничением, а не обработка сигнала. Фильтров
/// (`highpass`, `speechnorm`) здесь нет и быть не может — это и есть честная
/// деградация: транскрипт будет, качество чуть хуже.
pub fn mix_tracks_in_memory(
    tracks: &[MixTrack],
    sample_rate: u32,
    out: &Path,
) -> std::io::Result<u64> {
    use crate::ingest::wav::{read_samples, WavWriter};

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut acc: Vec<i32> = Vec::new();
    for t in tracks {
        let samples = read_samples(&t.path)?;
        let shift = (t.delay_ms as usize) * sample_rate as usize / 1000;
        let need = shift + samples.len();
        if acc.len() < need {
            acc.resize(need, 0);
        }
        for (i, s) in samples.iter().enumerate() {
            acc[shift + i] += i32::from(*s);
        }
    }

    let mixed: Vec<i16> = acc
        .into_iter()
        // Ограничение, а не деление на число дорожек: делить — значит
        // повторить ровно ту ошибку, ради которой в фильтрографе стоит
        // `normalize=0`.
        .map(|v| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
        .collect();

    let mut w = WavWriter::create(out, sample_rate)?;
    w.write_frame(0, &mixed)?;
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(n: usize, delay: u64) -> MixTrack {
        MixTrack {
            path: PathBuf::from(format!("/m/raw/track-{n}.wav")),
            delay_ms: delay,
        }
    }

    /// GOLDEN: полная строка фильтрографа для одной дорожки.
    #[test]
    fn the_filtergraph_for_one_track_is_exactly_this() {
        let got = build_mixdown_filtergraph(&[track(0, 0)]);
        assert_eq!(
            got,
            "[0:a]aresample=async=1:first_pts=0,adelay=0:all=1,highpass=f=80,speechnorm[a0];\
             [a0]amix=inputs=1:duration=longest:normalize=0[mix]"
        );
    }

    /// GOLDEN: две дорожки, вторая со сдвигом.
    #[test]
    fn the_filtergraph_for_two_tracks_is_exactly_this() {
        let got = build_mixdown_filtergraph(&[track(0, 0), track(1, 2_500)]);
        assert_eq!(
            got,
            "[0:a]aresample=async=1:first_pts=0,adelay=0:all=1,highpass=f=80,speechnorm[a0];\
             [1:a]aresample=async=1:first_pts=0,adelay=2500:all=1,highpass=f=80,speechnorm[a1];\
             [a0][a1]amix=inputs=2:duration=longest:normalize=0[mix]"
        );
    }

    /// GOLDEN: пять дорожек — тот случай, ради которого `normalize=0` и нужен.
    #[test]
    fn the_filtergraph_for_five_tracks_is_exactly_this() {
        let tracks: Vec<MixTrack> = (0..5).map(|i| track(i, i as u64 * 1_000)).collect();
        let got = build_mixdown_filtergraph(&tracks);
        let expected = "[0:a]aresample=async=1:first_pts=0,adelay=0:all=1,highpass=f=80,speechnorm[a0];\
[1:a]aresample=async=1:first_pts=0,adelay=1000:all=1,highpass=f=80,speechnorm[a1];\
[2:a]aresample=async=1:first_pts=0,adelay=2000:all=1,highpass=f=80,speechnorm[a2];\
[3:a]aresample=async=1:first_pts=0,adelay=3000:all=1,highpass=f=80,speechnorm[a3];\
[4:a]aresample=async=1:first_pts=0,adelay=4000:all=1,highpass=f=80,speechnorm[a4];\
[a0][a1][a2][a3][a4]amix=inputs=5:duration=longest:normalize=0[mix]";
        assert_eq!(got, expected);
        assert_eq!(build_mixdown_filtergraph(&[]), "", "сводить нечего");
    }

    /// **РЕГРЕСС: `normalize=0` стоит при ЛЮБОМ числе входов.**
    ///
    /// Отдельным тестом, а не «оно же в golden»: golden падает от любой
    /// правки цепочки, и в шуме его падения потеря `normalize=0` прошла бы
    /// незамеченной. А потеря эта тихая — файл получается, просто в N раз
    /// тише, и виден это только по качеству распознавания.
    #[test]
    fn normalize_zero_is_present_for_any_number_of_inputs() {
        for n in 1..=12usize {
            let tracks: Vec<MixTrack> = (0..n).map(|i| track(i, 0)).collect();
            let g = build_mixdown_filtergraph(&tracks);
            assert!(
                g.contains(&format!("amix=inputs={n}:duration=longest:normalize=0")),
                "n={n}: {g}"
            );
            assert!(!g.contains("normalize=1"), "n={n}: {g}");
            // И ни одного amix без normalize.
            assert_eq!(g.matches("amix=").count(), 1);
            assert_eq!(g.matches("normalize=0").count(), 1);
        }
    }

    /// `aresample=async=1` есть у КАЖДОЙ дорожки: он заполняет тишиной дыры
    /// от потерянных пакетов, и пропуск его у одной дорожки увёл бы её
    /// таймлайн относительно остальных.
    #[test]
    fn every_track_gets_aresample_and_the_speech_filters() {
        let tracks: Vec<MixTrack> = (0..4).map(|i| track(i, 0)).collect();
        let g = build_mixdown_filtergraph(&tracks);
        assert_eq!(g.matches("aresample=async=1:first_pts=0").count(), 4);
        assert_eq!(g.matches("highpass=f=80").count(), 4);
        assert_eq!(g.matches("speechnorm").count(), 4);
        assert_eq!(g.matches("adelay=").count(), 4);
    }

    fn state(idx: usize, start: u64) -> TrackState {
        TrackState {
            track_id: format!("t{idx}"),
            idx,
            start_offset_ms: start,
            end_offset_ms: None,
            frames: 1,
            samples: 320,
            end_samples: 320,
            capture_path: None,
        }
    }

    /// `adelay` берётся из `start_offset_ms` состояния дорожек — но только
    /// там, где файл начинается со своего нуля.
    #[test]
    fn the_delay_comes_from_the_track_state_only_when_the_file_starts_at_its_own_zero() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        for n in 0..2 {
            std::fs::write(raw.join(format!("track-{n}.wav")), b"RIFF").unwrap();
        }
        let states = [state(0, 0), state(1, 900_000)];

        let own = mix_tracks(&states, &raw, TrackOrigin::OwnZero);
        assert_eq!(own.len(), 2);
        assert_eq!(own[0].delay_ms, 0);
        assert_eq!(own[1].delay_ms, 900_000, "поздняя дорожка обязана сдвинуться");

        let shared = mix_tracks(&states, &raw, TrackOrigin::SharedT0);
        assert_eq!(shared[1].delay_ms, 0, "файл уже на общей оси — второй сдвиг сломал бы её");

        // Файла нет — дорожки нет: ffmpeg на отсутствующем входе падает
        // целиком, а одна пропавшая дорожка не повод потерять встречу.
        let missing = mix_tracks(&[state(7, 0)], &raw, TrackOrigin::SharedT0);
        assert!(missing.is_empty());
    }

    /// **ДОКАЗАТЕЛЬСТВО выбора `SharedT0`**: файл, записанный настоящим
    /// рекордером, УЖЕ содержит ведущую тишину до своего `start_offset_ms`.
    ///
    /// Тест читает результат `ingest::Recorder`, а не рассуждает о нём:
    /// поменяется адресация кадров — тест покраснеет, и `TrackOrigin` придётся
    /// пересмотреть осознанно.
    #[test]
    fn the_recorder_writes_files_that_already_sit_on_the_shared_axis() {
        use crate::bot::transport::PageMessage;
        use crate::ingest::{wav::read_samples, Manifest, Recorder};

        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::new("m1", "u", "telemost", "normal", 16_000);
        let mut r = Recorder::create(dir.path(), manifest).unwrap();
        // Дорожка появилась на 2,5 секунде.
        let start_samples = 16_000 * 2_500 / 1_000;
        r.accept(PageMessage::Audio(crate::bot::AudioFrame::new(
            "late",
            start_samples,
            16_000,
            vec![500i16; 320],
        )))
        .unwrap();
        r.finish().unwrap();

        let samples = read_samples(&dir.path().join("raw/track-0.wav")).unwrap();
        assert_eq!(samples.len(), start_samples as usize + 320);
        assert!(
            samples[..start_samples as usize].iter().all(|&s| s == 0),
            "файл обязан начинаться тишиной до start_offset_ms"
        );
    }

    #[test]
    fn the_mixdown_argv_asks_for_pcm_and_one_channel() {
        let out = PathBuf::from("/m/work/mix.wav");
        let args = build_mixdown_args(&[track(0, 0), track(1, 100)], &out, 16_000);
        let joined = args.join(" ");
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 2);
        assert!(joined.contains("-filter_complex"));
        assert!(joined.contains("-map [mix]"));
        assert!(joined.contains("-c:a pcm_s16le"));
        assert!(joined.contains("-ar 16000 -ac 1"));
        assert!(joined.ends_with("/m/work/mix.wav"));
        assert!(joined.contains("-nostdin"), "без -nostdin ffmpeg ждёт клавиатуру");
    }

    /// Архив — именно AAC 32 кбит/с моно с `+faststart`.
    #[test]
    fn the_archive_argv_is_aac_32k_mono_with_faststart() {
        let args = build_archive_args(Path::new("/m/work/mix.wav"), Path::new("/m/audio.m4a"));
        let joined = args.join(" ");
        assert!(joined.contains("-c:a aac"));
        assert!(joined.contains("-b:a 32k"));
        assert!(joined.contains("-ac 1"));
        assert!(joined.contains("-movflags +faststart"));
        assert!(joined.ends_with("/m/audio.m4a"), "{joined}");
        assert!(!joined.contains(".wav\""), "архив не WAV: 115 МБ на час убьют плеер");
    }

    #[test]
    fn ffprobe_duration_is_parsed_and_nonsense_is_rejected() {
        assert_eq!(parse_duration_secs("12.5\n"), Some(12.5));
        assert_eq!(parse_duration_secs("\n  3600.000000  \n"), Some(3600.0));
        assert_eq!(parse_duration_secs("N/A"), None);
        assert_eq!(parse_duration_secs(""), None);
        assert_eq!(parse_duration_secs("-1"), None);
        let args = build_probe_args(Path::new("/m/audio.m4a"));
        assert!(args.join(" ").contains("format=duration"));
    }

    /// СВЕДЕНИЕ БЕЗ FFMPEG: дорожки складываются, длина по самой длинной.
    #[test]
    fn the_in_memory_mixer_sums_tracks_and_keeps_the_longest() {
        use crate::ingest::wav::{read_samples, WavWriter};
        let dir = tempfile::tempdir().unwrap();

        let a = dir.path().join("a.wav");
        let mut w = WavWriter::create(&a, 16_000).unwrap();
        w.write_frame(0, &vec![100i16; 1_000]).unwrap();
        w.finish().unwrap();

        let b = dir.path().join("b.wav");
        let mut w = WavWriter::create(&b, 16_000).unwrap();
        w.write_frame(0, &vec![50i16; 2_000]).unwrap();
        w.finish().unwrap();

        let out = dir.path().join("work/mix.wav");
        let n = mix_tracks_in_memory(
            &[
                MixTrack { path: a, delay_ms: 0 },
                MixTrack { path: b, delay_ms: 0 },
            ],
            16_000,
            &out,
        )
        .unwrap();
        assert_eq!(n, 2_000, "длина по самой длинной дорожке");

        let mixed = read_samples(&out).unwrap();
        assert_eq!(mixed.len(), 2_000);
        assert_eq!(mixed[0], 150, "дорожки обязаны складываться");
        assert_eq!(mixed[1_999], 50, "хвост длинной дорожки на месте");
    }

    /// Сложение НЕ ДЕЛИТ на число дорожек — это была бы та самая ошибка,
    /// против которой в фильтрографе стоит `normalize=0`.
    #[test]
    fn the_in_memory_mixer_never_divides_by_the_number_of_tracks() {
        use crate::ingest::wav::{read_samples, WavWriter};
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..5 {
            let p = dir.path().join(format!("t{i}.wav"));
            let mut w = WavWriter::create(&p, 16_000).unwrap();
            w.write_frame(0, &vec![1_000i16; 100]).unwrap();
            w.finish().unwrap();
            paths.push(MixTrack { path: p, delay_ms: 0 });
        }
        let out = dir.path().join("mix.wav");
        mix_tracks_in_memory(&paths, 16_000, &out).unwrap();
        let mixed = read_samples(&out).unwrap();
        assert_eq!(mixed[0], 5_000, "громкость поделена на число дорожек");
    }

    /// Перегрузка ограничивается, а не заворачивается: `i16` в обёртке даёт
    /// щелчок на каждом сэмпле, и распознавание на таком звуке рассыпается.
    #[test]
    fn the_in_memory_mixer_clips_instead_of_wrapping() {
        use crate::ingest::wav::{read_samples, WavWriter};
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..3 {
            let p = dir.path().join(format!("loud{i}.wav"));
            let mut w = WavWriter::create(&p, 16_000).unwrap();
            w.write_frame(0, &vec![30_000i16; 10]).unwrap();
            w.finish().unwrap();
            paths.push(MixTrack { path: p, delay_ms: 0 });
        }
        let out = dir.path().join("mix.wav");
        mix_tracks_in_memory(&paths, 16_000, &out).unwrap();
        let mixed = read_samples(&out).unwrap();
        assert_eq!(mixed[0], i16::MAX, "получилось {}", mixed[0]);
    }

    /// Задержка в своём миксере работает так же, как `adelay` в ffmpeg.
    #[test]
    fn the_in_memory_mixer_honours_the_delay() {
        use crate::ingest::wav::{read_samples, WavWriter};
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("late.wav");
        let mut w = WavWriter::create(&p, 16_000).unwrap();
        w.write_frame(0, &vec![777i16; 160]).unwrap();
        w.finish().unwrap();

        let out = dir.path().join("mix.wav");
        mix_tracks_in_memory(
            &[MixTrack { path: p, delay_ms: 1_000 }],
            16_000,
            &out,
        )
        .unwrap();
        let mixed = read_samples(&out).unwrap();
        assert_eq!(mixed.len(), 16_000 + 160);
        assert!(mixed[..16_000].iter().all(|&s| s == 0));
        assert_eq!(mixed[16_000], 777);
    }
}
