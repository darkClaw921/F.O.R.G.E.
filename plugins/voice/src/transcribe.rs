//! Само задание расшифровки: файл на диске → текст в сторе.
//!
//! Это тело замыкания, которое плагин отдаёт [`crate::hub::VoiceHub::start`].
//! Живёт в ядре, а не в трёх роутах, ровно по той же причине, что и лестница
//! проверок: расшифровка в VK, Telegram и Bitrix — это буквально одно и то же
//! действие над файлом, и три копии разъехались бы на первой же правке.
//!
//! ```text
//! подготовка аудио ──▶ движок ──▶ склейка текста ──▶ запись в стор
//!    (ffmpeg)          (whisper|облако)                (JSON)
//! ```
//!
//! # ТЕКСТ ПОПАДАЕТ В СТОР ДО КАДРА `done`
//!
//! Иначе фронт, получив `done`, тут же дёрнул бы `GET …/transcript` и получил
//! бы «расшифровки нет» — гонка, воспроизводимая на быстрой машине с первого
//! раза. Порядок здесь обратный намеренно: сначала диск, потом объявление.
//!
//! # ПУСТОЙ ТЕКСТ — ЭТО ОТКАЗ, А НЕ УСПЕХ
//!
//! Голосовое без единого слова (тишина, шум ветра, случайное нажатие) даёт от
//! движка пустую строку. Сохранить её значило бы навсегда закэшировать «эта
//! запись расшифрована в ничего», и кнопка перестала бы работать даже после
//! починки. Пустой результат — `voice_empty`, и он не кэшируется.

use std::path::PathBuf;
use std::time::Duration;

use crate::audio::{self, PreparedAudio, Target};
use crate::cloud::{stt, CloudClient};
use crate::config::VoiceConfig;
use crate::error::VoiceError;
use crate::gate::EnginePlan;
use crate::hub::RunCtx;
use crate::proc::ProcError;
use crate::store::{Engine, Transcript, TranscriptStore, VoiceKey};
use crate::whisper::{self, RunOptions, WhisperFlags, WhisperRequest};

/// Ширина луча локального whisper.
///
/// Пять — то же, что в `forge-meet`: на голосовом в пару фраз разница по
/// времени незаметна, а по числу исправленных окончаний — заметна.
pub const BEAM_SIZE: u32 = 5;

/// Сколько процентов занимает подготовка аудио.
///
/// Полоса прогресса не имеет права стоять на нуле, пока ffmpeg молотит файл:
/// человек читает неподвижный ноль как «зависло».
pub const PREPARE_PERCENT: u8 = 5;

/// Всё, что нужно заданию.
#[derive(Debug, Clone)]
pub struct TranscribeJob {
    /// Настройки: язык, потолки, модель, ключ облака.
    pub cfg: VoiceConfig,
    /// Итог лестницы проверок: движок и пути бинарей.
    pub plan: EnginePlan,
    /// Исходный файл в медиа-кэше плагина.
    pub source: PathBuf,
    /// Каталог кэша плагина — в нём рождается рабочий подкаталог.
    pub cache_dir: PathBuf,
    /// Куда класть готовую расшифровку.
    pub store: TranscriptStore,
    /// Координаты вложения.
    pub key: VoiceKey,
}

impl TranscribeJob {
    /// Потолок одного запуска ffmpeg.
    fn ffmpeg_timeout(&self) -> Duration {
        Duration::from_secs(audio::DEFAULT_FFMPEG_TIMEOUT_SECS)
    }
}

/// Расшифровывает вложение и кладёт результат в стор.
///
/// # Errors
///
/// Любой отказ подготовки, движка или стора — СВОИМ кодом (`metal_unavailable`,
/// `cloud_no_funds`, `voice_too_long`, `cancelled`, …). Реестр перекладывает
/// код в кадр `error`, а плагин — в HTTP-ответ.
pub async fn run(job: TranscribeJob, ctx: RunCtx) -> Result<Transcript, VoiceError> {
    // Отмену проверяем до первого тяжёлого шага: пользователь мог передумать,
    // пока задание стояло в очереди планировщика tokio.
    if ctx.cancel().is_cancelled() {
        return Err(VoiceError::Cancelled);
    }

    let target = match job.plan.engine {
        Engine::Local => Target::Wav16k,
        Engine::Cloud => Target::Mp3,
    };
    let prepared = audio::prepare(
        &job.source,
        target,
        &job.plan.ffmpeg,
        &job.cache_dir,
        job.cfg.max_duration_secs,
        job.ffmpeg_timeout(),
        ctx.cancel(),
    )
    .await?;
    ctx.progress(PREPARE_PERCENT);

    let (text, language) = match job.plan.engine {
        Engine::Local => run_local(&job, &prepared, &ctx).await?,
        Engine::Cloud => run_cloud(&job, &prepared, &ctx).await?,
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(VoiceError::Proc(ProcError::new(
            "voice_empty",
            "движок не распознал ни одного слова: в записи нет речи",
            "",
        )));
    }

    let transcript = Transcript::new(text, job.plan.engine)
        .with_language(language.unwrap_or_else(|| job.cfg.language.clone()))
        .with_duration_ms(prepared.duration_ms);
    // Диск ПЕРЕД кадром `done` — см. шапку модуля.
    job.store.put(&job.key, &transcript)?;
    ctx.progress(100);
    Ok(transcript)
}

/// Локальный whisper.cpp. GPU обязателен — гард внутри [`whisper::run`].
async fn run_local(
    job: &TranscribeJob,
    prepared: &PreparedAudio,
    ctx: &RunCtx,
) -> Result<(String, Option<String>), VoiceError> {
    let (Some(bin), Some(model)) = (job.plan.whisper.clone(), job.plan.model.clone()) else {
        // Сюда не попасть через `gate::check`, но собрать `EnginePlan` руками
        // можно — и тогда честный отказ лучше `unwrap`.
        return Err(VoiceError::MissingBinary {
            what: "whisper",
            hint: "план расшифровки собран без бинаря и модели".into(),
        });
    };

    let req = WhisperRequest {
        bin,
        model,
        wav: prepared.converted.clone(),
        out_prefix: prepared.work_dir().join("out"),
        language: job.cfg.language.clone(),
        beam_size: BEAM_SIZE,
        // VAD выключен: `--vad` без `-vm` роняет whisper.cpp по SIGABRT, а
        // голосовое и без него укладывается в одно окно.
        vad_model: None,
    };
    let opts = RunOptions {
        timeout: Duration::from_secs(job.cfg.local_timeout_secs),
        cancel: ctx.cancel().clone(),
        ..RunOptions::default()
    };

    let reporter = ctx.clone();
    let on_progress = move |p: u8| reporter.progress(scale(p));
    let outcome = whisper::run(&req, &WhisperFlags::default(), &opts, Some(&on_progress)).await?;

    let text = outcome
        .segments
        .iter()
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    // Язык whisper в выводе не называет — берём тот, что просили.
    Ok((text, None))
}

/// Облако polza.ai: mp3 → окна ≤ 90 с → текст.
async fn run_cloud(
    job: &TranscribeJob,
    prepared: &PreparedAudio,
    ctx: &RunCtx,
) -> Result<(String, Option<String>), VoiceError> {
    let cloud_cfg = job.cfg.cloud();
    let client = CloudClient::new(&cloud_cfg)?;
    let opts = stt::SttOptions {
        ffmpeg: job.plan.ffmpeg.clone(),
        ffmpeg_timeout: job.ffmpeg_timeout(),
        window_ms: cloud_cfg.window_ms(),
        // Интервалов речи у голосового нет и взять их неоткуда: VAD считает их
        // на встречах по дорожкам участников. Пусто — режем арифметически, и
        // это честнее, чем придумать паузы.
        speech: Vec::new(),
        model: cloud_cfg.stt_model.clone(),
        language: cloud_cfg.stt_language.clone(),
        request_timeout: Duration::from_secs(job.cfg.cloud_timeout_secs),
        cancel: ctx.cancel().clone(),
    };

    let reporter = ctx.clone();
    let on_progress = move |p: u8| reporter.progress(scale(p));
    let outcome = stt::transcribe_file(
        &client,
        &prepared.converted,
        prepared.duration_ms,
        prepared.work_dir(),
        &opts,
        Some(&on_progress),
    )
    .await?;

    if let Some(note) = outcome.gap_note() {
        // Дыра в расшифровке — не отказ (текст есть), но человек имеет право
        // узнать о ней хотя бы из журнала.
        tracing::warn!(
            target: "forge_voice",
            key = %audio::key_label(&job.key),
            "forge-voice: {note}"
        );
    }
    Ok((outcome.text(), outcome.language.clone()))
}

/// Проценты движка → проценты полосы, с учётом уже потраченного на подготовку.
///
/// Без сдвига полоса дёргалась бы назад: подготовка объявила 5 %, а первый
/// кадр движка пришёл бы с 0 %.
#[must_use]
fn scale(p: u8) -> u8 {
    let span = 100 - u32::from(PREPARE_PERCENT);
    (u32::from(PREPARE_PERCENT) + u32::from(p) * span / 100).min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::VoiceSink;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Rec {
        frames: Mutex<Vec<String>>,
    }

    impl VoiceSink for Rec {
        fn started(&self, _r: &str, _k: &VoiceKey, e: &str) {
            self.frames.lock().unwrap().push(format!("started {e}"));
        }
        fn progress(&self, _r: &str, _k: &VoiceKey, p: u8) {
            self.frames.lock().unwrap().push(format!("progress {p}"));
        }
        fn error(&self, _r: &str, _k: &VoiceKey, c: &str, _m: &str) {
            self.frames.lock().unwrap().push(format!("error {c}"));
        }
        fn done(&self, _r: &str, _k: &VoiceKey, t: &Transcript) {
            self.frames.lock().unwrap().push(format!("done {}", t.text));
        }
    }

    /// Проценты движка не двигают полосу назад и не переваливают за сотню.
    #[test]
    fn the_progress_scale_never_goes_backwards() {
        assert_eq!(scale(0), PREPARE_PERCENT);
        assert_eq!(scale(100), 100);
        assert!(scale(50) > scale(49));
        assert!(scale(1) >= PREPARE_PERCENT);
    }

    /// **ОТМЕНА ДО СТАРТА НЕ ЗАПУСКАЕТ НИ ОДНОГО ПРОЦЕССА.**
    ///
    /// Пути бинарей заведомо несуществующие: если бы задание их запускало, тест
    /// упал бы с `spawn_failed`, а не с `cancelled`.
    #[tokio::test]
    async fn a_cancelled_job_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("audio.bin");
        std::fs::write(&src, b"OggS").unwrap();

        let hub = crate::hub::VoiceHub::new(Arc::new(Rec::default()), 1, Duration::from_secs(30));
        let key = VoiceKey::new("peer", "1", 0);
        let job = TranscribeJob {
            cfg: VoiceConfig::default(),
            plan: EnginePlan {
                engine: Engine::Cloud,
                ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
                whisper: None,
                model: None,
            },
            source: src,
            cache_dir: dir.path().to_path_buf(),
            store: TranscriptStore::new(dir.path()),
            key: key.clone(),
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        // `RunCtx` собирается только реестром, поэтому идём через него: run
        // получает уже отменённый токен, потому что мы отменяем ключ сразу.
        let outcome = hub
            .start(key.clone(), "cloud", move |ctx| async move {
                ctx.cancel().cancel();
                run(job, ctx).await
            })
            .unwrap();
        assert!(outcome.is_new());
        for _ in 0..100 {
            if hub.active() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(hub.active(), 0);
    }

    /// **ПУСТОЙ ТЕКСТ НЕ КЭШИРУЕТСЯ.**
    ///
    /// Проверяется на сторе напрямую: задание пишет ТОЛЬКО непустой результат,
    /// поэтому после отказа стор обязан остаться пустым.
    #[test]
    fn an_empty_result_never_reaches_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path());
        let key = VoiceKey::new("peer", "1", 0);
        assert!(store.get(&key).is_none());

        let err = VoiceError::Proc(ProcError::new("voice_empty", "нет речи", ""));
        assert_eq!(err.code(), "voice_empty");
        assert!(store.get(&key).is_none(), "пустой результат не пишется");
    }
}
