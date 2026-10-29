//! Подготовка аудио к расшифровке: файл из медиа-кэша → длительность → формат,
//! который примет движок.
//!
//! Модуль общий на три мессенджера и НЕ ЗНАЕТ, какой из них его позвал: путь к
//! каталогу кэша, имя каталога беседы и основу имени файла передают
//! параметрами. Знание, которое здесь всё-таки живёт, — это РАСКЛАДКА, общая у
//! всех трёх (`plugins/*/src/media_cache.rs`):
//!
//! ```text
//! <cache_dir>/media/<каталог беседы>/<основа>.bin        ← байты
//! <cache_dir>/media/<каталог беседы>/<основа>.meta.json  ← метаданные
//! ```
//!
//! Основа различается — у VK это `<mid>.<idx>.full`, у Telegram `<mid>.full`
//! (у сообщения ровно одно вложение), у Bitrix `<mid>.<idx>.file`, — поэтому
//! её собирает плагин из своего `CacheKey`, а модуль дописывает только `.bin`.
//!
//! # MP3 ПЕРЕД ОБЛАКОМ — НЕ ОПЦИЯ
//!
//! Шлюз polza.ai роняет запрос с WAV-телом: измерено на `forge-meet`, там же
//! из-за этого нарезка окон сразу кодирует в mp3
//! ([`crate::cloud::stt::build_window_args`]). Голосовые приходят в ogg/opus
//! (Telegram, VK) и m4a (Bitrix), то есть даже «просто переслать как есть»
//! означало бы отправить в облако три разных контейнера и выяснять их судьбу
//! по одному. Один формат на входе — одна история отказов.
//!
//! # WAV ПЕРЕД ЛОКАЛЬНЫМ WHISPER — ТОЖЕ НЕ ОПЦИЯ
//!
//! `whisper-cli` читает только WAV (ogg и m4a он не открывает вовсе), причём
//! хочет 16 кГц моно. Это ровно та частота, что в
//! `plugins/meet/src/config.rs::AUDIO_SAMPLE_RATE`, и совпадение не случайно:
//! пересэмплирование — самый дорогой кусок конвейера.
//!
//! # ВРЕМЕННЫЕ ФАЙЛЫ УБИРАЮТСЯ ПРИ ЛЮБОМ ИСХОДЕ
//!
//! Конвертация пишет на диск столько же, сколько весит запись, и делает это на
//! КАЖДОЕ нажатие «расшифровать». Мусор здесь копился бы тихо: каталог кэша
//! чистит LRU медиа, а про чужие временные файлы он ничего не знает. Поэтому
//! рабочий каталог живёт в [`ScratchDir`], который сносит себя на `Drop` — в
//! том числе при ошибке, отмене и панике.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::error::VoiceError;
use crate::proc::{self, ProcError};
use crate::store::VoiceKey;
use crate::waveform::{decode_s16le, DECODE_SAMPLE_RATE};

/// Подкаталог медиа-кэша плагина. Одинаков у всех трёх.
pub const MEDIA_SUBDIR: &str = "media";

/// Подкаталог для рабочих файлов расшифровки внутри каталога кэша.
///
/// Внутри каталога кэша, а не в `/tmp`: `rename` между файловыми системами не
/// атомарен, а `/tmp` вполне может оказаться другой ФС — то же соображение, по
/// которому staging медиа-кэша лежит рядом с целью.
pub const SCRATCH_SUBDIR: &str = "voice-work";

/// Битрейт mp3 для облака.
///
/// 64 кбит/с моно: речь на этом битрейте неотличима на слух, а размер запроса
/// втрое меньше, чем у 192. Окна `forge-meet` жмутся ещё сильнее (32 кбит/с),
/// но там их десятки на встречу; голосовое одно, и лишние секунды загрузки
/// дешевле, чем риск потерять согласные.
pub const MP3_BITRATE: &str = "64k";

/// Частота дискретизации для локального whisper.cpp.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Потолок одного запуска ffmpeg, секунды.
///
/// Три минуты: перекодирование пятнадцатиминутного голосового занимает единицы
/// секунд, и всё, что дольше, означает зависший процесс, а не длинную запись.
pub const DEFAULT_FFMPEG_TIMEOUT_SECS: u64 = 180;

/// В какой формат готовим запись.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `mp3` для облака polza.ai.
    Mp3,
    /// WAV 16 кГц моно для локального whisper.cpp.
    Wav16k,
}

impl Target {
    /// Расширение файла.
    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav16k => "wav",
        }
    }
}

// --- раскладка медиа-кэша ------------------------------------------------------

/// Каталог беседы в медиа-кэше плагина.
#[must_use]
pub fn media_dir(cache_dir: &Path, dir_name: &str) -> PathBuf {
    cache_dir.join(MEDIA_SUBDIR).join(dir_name)
}

/// Путь байтов вложения: `<cache>/media/<беседа>/<основа>.bin`.
#[must_use]
pub fn media_bin(cache_dir: &Path, dir_name: &str, stem: &str) -> PathBuf {
    media_dir(cache_dir, dir_name).join(format!("{stem}.bin"))
}

/// Ищет вложение в кэше. `None` — файла нет либо он пуст.
///
/// Пустой файл — это не «есть»: так выглядит оборванная запись, и отдавать его
/// движку значило бы получить невнятный отказ ffmpeg вместо честного
/// «скачайте заново».
#[must_use]
pub fn resolve_media(cache_dir: &Path, dir_name: &str, stem: &str) -> Option<PathBuf> {
    let path = media_bin(cache_dir, dir_name, stem);
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() && m.len() > 0 => Some(path),
        _ => None,
    }
}

/// Проверяет, что файл существует и не пуст.
///
/// # Errors
///
/// [`VoiceError::Audio`] (код `voice_audio`).
pub fn require_readable(path: &Path) -> Result<(), VoiceError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| VoiceError::Audio(format!("{} не читается: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(VoiceError::Audio(format!("{} не файл", path.display())));
    }
    if meta.len() == 0 {
        return Err(VoiceError::Audio(format!(
            "{} пуст: расшифровывать нечего",
            path.display()
        )));
    }
    Ok(())
}

// --- рабочий каталог -----------------------------------------------------------

/// Уникальный каталог, который сносит себя на `Drop`.
///
/// Имя содержит PID **и** счётчик процесса: только PID недостаточно — два
/// потока одного devforge, расшифровывающие разные голосовые, получили бы одно
/// имя и стёрли бы друг другу файлы. Ровно на этом уже обжигался
/// `forge_atomic::tmp_path_for` (см. `forge-vcqz7`).
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Создаёт каталог внутри `<cache_dir>/voice-work/`.
    ///
    /// # Errors
    ///
    /// [`VoiceError::Storage`], если каталог не создаётся (нет прав, диск
    /// переполнен).
    pub fn new(cache_dir: &Path) -> Result<Self, VoiceError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = cache_dir
            .join(SCRATCH_SUBDIR)
            .join(format!("run-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|e| {
            VoiceError::Storage(format!(
                "рабочий каталог {} не создаётся: {e}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }

    /// Путь каталога.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Путь файла внутри каталога.
    #[must_use]
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Ошибку глотаем осознанно: `Drop` бежит и на пути отмены, и на пути
        // паники, а вторая ошибка поверх первой ничего не чинит. Остаток
        // подметёт LRU-подчистка кэша плагина.
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(
                    target: "forge_voice",
                    path = %self.path.display(),
                    "forge-voice: рабочий каталог не удалился: {e}"
                );
            }
        }
    }
}

// --- ffmpeg --------------------------------------------------------------------

/// Общая часть argv ffmpeg.
///
/// `-nostdin` обязателен: без него ffmpeg на неоднозначном вопросе («файл
/// существует, перезаписать?») ждёт ответа с клавиатуры и висит до таймаута.
/// `-y` убирает сам вопрос, `-v error` — баннер и таблицу кодеков, которые
/// иначе забивают хвост stderr в сообщении об ошибке.
fn base_args(input: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        input.display().to_string(),
    ]
}

/// argv перекодирования в mp3 для облака. **Чистая функция.**
#[must_use]
pub fn build_mp3_args(input: &Path, out: &Path) -> Vec<String> {
    let mut args = base_args(input);
    args.extend([
        // Моно: у голосового второй канал — копия первого, и стерео удвоило бы
        // размер запроса, не добавив ни одного слова.
        "-ac".into(),
        "1".into(),
        "-c:a".into(),
        "libmp3lame".into(),
        "-b:a".into(),
        MP3_BITRATE.into(),
        out.display().to_string(),
    ]);
    args
}

/// argv перекодирования в WAV 16 кГц моно для whisper.cpp. **Чистая функция.**
#[must_use]
pub fn build_wav_args(input: &Path, out: &Path) -> Vec<String> {
    let mut args = base_args(input);
    args.extend([
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        WHISPER_SAMPLE_RATE.to_string(),
        // Явный кодек: контейнер `.wav` допускает и float, и ADPCM, а
        // whisper.cpp читает только 16-битный PCM.
        "-c:a".into(),
        "pcm_s16le".into(),
        out.display().to_string(),
    ]);
    args
}

/// argv нужного формата.
#[must_use]
pub fn build_args(target: Target, input: &Path, out: &Path) -> Vec<String> {
    match target {
        Target::Mp3 => build_mp3_args(input, out),
        Target::Wav16k => build_wav_args(input, out),
    }
}

/// Перекодирует файл. Отмена и таймаут доходят до процесса.
///
/// # Errors
///
/// [`VoiceError::Proc`]: `spawn_failed` (бинаря нет — но до сюда это обычно не
/// доходит, см. [`crate::tools::resolve_ffmpeg`]), `exit_failed` (ffmpeg не
/// понял вход), `timeout`, `cancelled`. Плюс [`VoiceError::Audio`], если
/// ffmpeg отчитался успехом, а файла нет или он пуст — так выглядит
/// «расшифровали тишину», и молчать об этом нельзя.
pub async fn convert(
    target: Target,
    input: &Path,
    out: &Path,
    ffmpeg: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), VoiceError> {
    let args = build_args(target, input, out);
    proc::run(ffmpeg, &args, timeout, cancel).await?;
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(VoiceError::Audio(format!(
            "ffmpeg отчитался успехом, но {} пуст: во вложении нет звуковой дорожки",
            out.display()
        ))),
    }
}

/// Длительность записи в миллисекундах.
///
/// Считается ДЕКОДИРОВАНИЕМ, а не разбором строки `Duration:` из stderr: у
/// ogg/opus, которым приходят голосовые Telegram и VK, заголовок регулярно
/// врёт (длительность в нём — оценка по битрейту), а расхождение здесь стоит
/// дорого: по этому числу проверяется потолок и планируются окна облака.
/// Декодируется 8 кГц моно — этого достаточно, чтобы посчитать сэмплы, и
/// вчетверо дешевле, чем декодировать в рабочую частоту.
///
/// # Errors
///
/// [`VoiceError::Proc`] от ffmpeg; [`VoiceError::Audio`], если во входе нет ни
/// одного сэмпла (файл не аудио, битый контейнер, «видео без звука»).
pub async fn probe_duration_ms(
    input: &Path,
    ffmpeg: &Path,
    timeout: Duration,
) -> Result<i64, VoiceError> {
    require_readable(input)?;
    let args = crate::waveform::build_decode_args(input, DECODE_SAMPLE_RATE);
    let pcm = proc::capture_stdout_bytes(ffmpeg, &args, timeout).await?;
    let samples = decode_s16le(&pcm).len() as i64;
    if samples == 0 {
        return Err(VoiceError::Audio(format!(
            "{}: ffmpeg не выдал ни одного сэмпла (это не аудио или файл битый)",
            input.display()
        )));
    }
    Ok(samples * 1000 / i64::from(DECODE_SAMPLE_RATE))
}

/// Готовая к расшифровке запись.
///
/// Держит [`ScratchDir`] внутри: пока значение живо, файл на диске существует,
/// а как только оно уронено — каталог снесён. Именно поэтому поле рабочего
/// каталога приватное: отдать его наружу значило бы разрешить пережить
/// собственные файлы.
#[derive(Debug)]
pub struct PreparedAudio {
    /// Исходный файл в медиа-кэше плагина (его НЕ трогаем).
    pub source: PathBuf,
    /// Перекодированный файл — то, что уедет движку.
    pub converted: PathBuf,
    /// Длительность записи, мс.
    pub duration_ms: i64,
    /// Рабочий каталог; сносится вместе со структурой.
    scratch: ScratchDir,
}

impl PreparedAudio {
    /// Рабочий каталог — движку он нужен под свои промежуточные файлы (окна
    /// облака, json локального whisper).
    #[must_use]
    pub fn work_dir(&self) -> &Path {
        self.scratch.path()
    }
}

/// Полная подготовка: проверка файла → длительность → перекодирование.
///
/// Порядок существен. Длительность считается ДО перекодирования, потому что
/// потолок длительности обязан отсекать двухчасовую «голосовуху» ДО того, как
/// мы потратим на неё минуту процессорного времени и гигабайт диска.
///
/// # Errors
///
/// [`VoiceError::TooLong`] с потолком; [`VoiceError::Audio`]; отказы ffmpeg
/// своими кодами. При любом из них рабочий каталог уже снесён.
pub async fn prepare(
    source: &Path,
    target: Target,
    ffmpeg: &Path,
    cache_dir: &Path,
    max_duration_secs: u64,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<PreparedAudio, VoiceError> {
    require_readable(source)?;
    let scratch = ScratchDir::new(cache_dir)?;

    let duration_ms = probe_duration_ms(source, ffmpeg, timeout).await?;
    let secs = duration_ms / 1000;
    if max_duration_secs > 0 && secs > max_duration_secs as i64 {
        return Err(VoiceError::TooLong {
            secs,
            limit: max_duration_secs,
        });
    }

    let converted = scratch.join(&format!("audio.{}", target.ext()));
    convert(target, source, &converted, ffmpeg, timeout, cancel).await?;

    Ok(PreparedAudio {
        source: source.to_path_buf(),
        converted,
        duration_ms,
        scratch,
    })
}

/// Основа имени файла расшифровки в журнале — для сообщений об ошибках.
///
/// Не путь: в журнал уезжают координаты вложения, а не место на диске.
#[must_use]
pub fn key_label(key: &VoiceKey) -> String {
    format!("{}/{}/{}", key.peer, key.message_id, key.index)
}

/// Ошибка ffmpeg, у которой не может быть кода: используется тестами и
/// вызывающими, которым нужен отказ «звука нет» без запуска процесса.
#[must_use]
pub fn no_audio_error(what: &str) -> VoiceError {
    VoiceError::Proc(ProcError::new(
        "no_audio",
        format!("расшифровывать нечего: {what}"),
        "",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Есть ли на машине настоящий ffmpeg. Тесты, которым он нужен, без него
    /// ПРОПУСКАЮТСЯ, а не падают: прогон в CI без медиатеки не должен быть
    /// красным по причине, к коду отношения не имеющей.
    fn ffmpeg() -> Option<PathBuf> {
        crate::tools::find_in_path("ffmpeg")
    }

    /// Делает WAV заданной длительности — вход для настоящих прогонов ffmpeg.
    fn write_wav(path: &Path, secs: f32) {
        let rate = 16_000u32;
        let n = (rate as f32 * secs) as u32;
        let data_len = n * 2;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // моно
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            // Синус 440 Гц: тишина прошла бы декодер, но не отличила бы
            // «посчитали сэмплы» от «получили нули».
            let t = i as f32 / rate as f32;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 8000.0;
            out.extend_from_slice(&(v as i16).to_le_bytes());
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, out).unwrap();
    }

    /// **РЕЗОЛВ ИЗ КЭША: ЕСТЬ, НЕТ, ПУСТ.**
    #[test]
    fn resolving_from_the_media_cache_covers_all_three_states() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        assert!(resolve_media(cache, "2000000042", "77.0.full").is_none());

        let path = media_bin(cache, "2000000042", "77.0.full");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(
            resolve_media(cache, "2000000042", "77.0.full").is_none(),
            "пустой файл не считается кэшем: так выглядит оборванная запись"
        );

        std::fs::write(&path, b"OggS...").unwrap();
        assert_eq!(resolve_media(cache, "2000000042", "77.0.full").unwrap(), path);
        // Раскладка ровно та же, что у медиа-кэша плагинов.
        assert!(path.ends_with("media/2000000042/77.0.full.bin"), "{path:?}");
    }

    /// Отсутствующий и пустой файл — внятная ошибка, а не паника.
    #[test]
    fn an_unreadable_source_is_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("нет.bin");
        let err = require_readable(&missing).unwrap_err();
        assert_eq!(err.code(), "voice_audio");
        assert!(err.to_string().contains("нет.bin"), "{err}");

        let empty = dir.path().join("пусто.bin");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(require_readable(&empty).unwrap_err().code(), "voice_audio");
    }

    /// **argv КОНВЕРТАЦИИ: MP3 ДЛЯ ОБЛАКА, WAV 16 кГц ДЛЯ WHISPER.**
    ///
    /// Тест на чистой функции, потому что именно здесь живёт измеренный факт:
    /// WAV роняет шлюз polza.ai, а whisper.cpp не читает ничего, кроме WAV.
    #[test]
    fn the_argv_pins_the_format_of_each_engine() {
        let mp3 = build_mp3_args(Path::new("/in.ogg"), Path::new("/out.mp3"));
        assert!(mp3.windows(2).any(|w| w == ["-c:a", "libmp3lame"]), "{mp3:?}");
        assert!(mp3.windows(2).any(|w| w == ["-b:a", MP3_BITRATE]), "{mp3:?}");
        assert!(mp3.windows(2).any(|w| w == ["-ac", "1"]), "{mp3:?}");
        assert_eq!(mp3.last().unwrap(), "/out.mp3");

        let wav = build_wav_args(Path::new("/in.ogg"), Path::new("/out.wav"));
        assert!(wav.windows(2).any(|w| w == ["-ar", "16000"]), "{wav:?}");
        assert!(wav.windows(2).any(|w| w == ["-c:a", "pcm_s16le"]), "{wav:?}");
        assert!(wav.windows(2).any(|w| w == ["-ac", "1"]), "{wav:?}");

        // `-nostdin` в обоих: без него ffmpeg ждёт клавиатуру и висит.
        for args in [&mp3, &wav] {
            assert!(args.contains(&"-nostdin".to_string()), "{args:?}");
            assert!(args.contains(&"-y".to_string()), "{args:?}");
        }
        assert_eq!(build_args(Target::Mp3, Path::new("/i"), Path::new("/o")).last().unwrap(), "/o");
        assert_eq!(Target::Mp3.ext(), "mp3");
        assert_eq!(Target::Wav16k.ext(), "wav");
    }

    /// **РАБОЧИЙ КАТАЛОГ СНОСИТСЯ НА `Drop`.**
    #[test]
    fn the_scratch_dir_removes_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = {
            let scratch = ScratchDir::new(dir.path()).unwrap();
            std::fs::write(scratch.join("audio.mp3"), "мусор".as_bytes()).unwrap();
            let p = scratch.path().to_path_buf();
            assert!(p.exists());
            p
        };
        assert!(!path.exists(), "рабочий каталог пережил владельца: {path:?}");

        // Два каталога подряд не совпадают: иначе два потока стёрли бы файлы
        // друг другу (та же ловушка, что у forge-atomic с одним лишь PID).
        let a = ScratchDir::new(dir.path()).unwrap();
        let b = ScratchDir::new(dir.path()).unwrap();
        assert_ne!(a.path(), b.path());
    }

    /// **ОТСУТСТВИЕ ffmpeg — ВНЯТНАЯ ОШИБКА, А НЕ ПАНИКА.**
    #[tokio::test]
    async fn a_missing_ffmpeg_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.wav");
        write_wav(&src, 0.1);
        let out = dir.path().join("out.mp3");

        let err = convert(
            Target::Mp3,
            &src,
            &out,
            Path::new("/nonexistent/ffmpeg"),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "spawn_failed", "{err}");

        let err = probe_duration_ms(&src, Path::new("/nonexistent/ffmpeg"), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "spawn_failed");
    }

    /// **НАСТОЯЩИЙ ПРОГОН: mp3 получается, длительность считается верно, и
    /// рабочий каталог не течёт.**
    #[tokio::test]
    async fn a_real_run_produces_an_mp3_and_a_correct_duration() {
        let Some(ffmpeg) = ffmpeg() else {
            eprintln!("ffmpeg не установлен — проба пропущена");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let src = media_bin(dir.path(), "peer", "1.0.full");
        write_wav(&src, 1.5);

        let prepared = prepare(
            &src,
            Target::Mp3,
            &ffmpeg,
            dir.path(),
            900,
            Duration::from_secs(60),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!((prepared.duration_ms - 1500).abs() <= 60, "{}", prepared.duration_ms);
        let bytes = std::fs::read(&prepared.converted).unwrap();
        assert!(!bytes.is_empty());
        // Заголовок mp3: либо тег ID3, либо синхрослово кадра.
        assert!(
            bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0),
            "это не mp3: {:02X?}",
            &bytes[..4]
        );
        assert!(prepared.work_dir().exists());

        let work = prepared.work_dir().to_path_buf();
        drop(prepared);
        assert!(!work.exists(), "временные файлы пережили подготовку");
        // И в каталоге кэша не осталось ни одного рабочего подкаталога.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(SCRATCH_SUBDIR))
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "мусор в рабочем каталоге: {leftovers:?}");
    }

    /// WAV-путь локального движка даёт ровно 16 кГц моно.
    #[tokio::test]
    async fn the_local_path_produces_16k_mono_wav() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.wav");
        write_wav(&src, 0.5);

        let prepared = prepare(
            &src,
            Target::Wav16k,
            &ffmpeg,
            dir.path(),
            900,
            Duration::from_secs(60),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let bytes = std::fs::read(&prepared.converted).unwrap();
        assert!(bytes.starts_with(b"RIFF"), "не WAV");
        // Частота лежит в заголовке по смещению 24.
        let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        assert_eq!(rate, WHISPER_SAMPLE_RATE);
        let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
        assert_eq!(channels, 1);
    }

    /// **ПОТОЛОК ДЛИТЕЛЬНОСТИ ОТСЕКАЕТ ДО ПЕРЕКОДИРОВАНИЯ И НЕ ОСТАВЛЯЕТ
    /// МУСОРА.**
    #[tokio::test]
    async fn a_too_long_recording_is_refused_before_conversion() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.wav");
        write_wav(&src, 2.0);

        let err = prepare(
            &src,
            Target::Mp3,
            &ffmpeg,
            dir.path(),
            1, // потолок — одна секунда
            Duration::from_secs(60),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "voice_too_long");
        assert!(err.to_string().contains('1'), "потолок обязан быть назван: {err}");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(SCRATCH_SUBDIR))
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "отказ оставил временные файлы: {leftovers:?}");
    }

    /// Файл не аудио — отказ ffmpeg своим кодом, и снова без мусора.
    #[tokio::test]
    async fn a_non_audio_file_is_refused_without_leaking_temp_files() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.bin");
        std::fs::write(&src, "это вообще не медиа".as_bytes()).unwrap();

        let err = prepare(
            &src,
            Target::Mp3,
            &ffmpeg,
            dir.path(),
            900,
            Duration::from_secs(60),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err.code(), "exit_failed" | "voice_audio"),
            "неожиданный код: {} ({err})",
            err.code()
        );

        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(SCRATCH_SUBDIR))
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// Отмена доходит до ffmpeg и не оставляет полуфайла.
    #[tokio::test]
    async fn cancelling_stops_the_conversion() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.wav");
        write_wav(&src, 0.2);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = convert(
            Target::Mp3,
            &src,
            &dir.path().join("out.mp3"),
            &ffmpeg,
            Duration::from_secs(30),
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "cancelled", "{err}");
    }

    /// Подпись вложения в журнале — координаты, а не путь на диске.
    #[test]
    fn the_key_label_is_coordinates_not_a_path() {
        let label = key_label(&VoiceKey::new("2000000042", "77", 1));
        assert_eq!(label, "2000000042/77/1");
        assert_eq!(no_audio_error("нулевая длительность").code(), "no_audio");
    }
}
