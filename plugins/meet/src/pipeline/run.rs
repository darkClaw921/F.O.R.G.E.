//! Сквозной прогон пост-обработки: от сырых WAV до `transcript.md`.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! ```text
//! transcoding ──► work/mix.wav ──► audio.m4a (архив, только с ffmpeg)
//!      │                │
//!      │                └──► ffprobe: дрейф таймлайна
//!      ▼
//! transcribing ──► ОЖИДАНИЕ конца записей ──► очередь whisper (1 слот)
//!      │
//!      ▼
//! attribute_segments ──► transcript.json + transcript.md
//!      │
//!      ▼
//! summarizing ──► очередь саммари (2 слота) ──► summary.md ──► ready
//! ```
//!
//! # ПОСЛЕ ТРАНСКРИПТА ВСТРЕЧА УЖЕ НЕ ПАДАЕТ
//!
//! Саммари — последний этап, и он единственный, чей провал НЕ переводит
//! встречу в `failed`: `summarizing → failed` запрещён таблицей
//! [`crate::status::can_transition`], а неудача Claude пишется в СВОЙ статус
//! ([`crate::status::SummaryStatus::Failed`]) при `ready` у самой встречи. Час
//! расшифровки не имеет права пропасть из-за неответившего саммаризатора.
//!
//! # Повтор этапа
//!
//! [`Stage`] — те самые значения параметра `from` у
//! `POST /api/meet/meetings/:id/retry`. Этапы идемпотентны по
//! детерминированным путям: повторный прогон переписывает те же файлы теми же
//! байтами (на это есть тест в [`super::render`]), поэтому повтор безопасен и
//! не требует предварительной уборки.
//!
//! # ДВА ЖЁСТКИХ ОГРАНИЧЕНИЯ ОЧЕРЕДИ
//!
//! **1. Слот whisper строго один, и поднять его нельзя.** Metal занимается
//! процессом целиком: два прогона дерутся за GPU и суммарно идут МЕДЛЕННЕЕ
//! одного. Переполнение — FIFO-очередь, а не отказ: запись уже на диске и
//! никуда не денется.
//!
//! **2. Whisper не запускается, пока идёт ЛЮБАЯ запись.** Chrome рендерит
//! встречу на том же GPU и начнёт получать дропы — то есть испортится звук
//! ровно у той встречи, которая пишется прямо сейчас. Отложенная расшифровка
//! стоит минут, испорченная запись — невосстановима.
//!
//! Очередь видна честно: статус остаётся `transcribing`, но кадр `progress`
//! несёт `queued: true` и `queue_position`. Отдельного статуса «в очереди»
//! нет — он размножил бы таблицу переходов вдвое ради временной информации.
//!
//! # Без ffmpeg встреча ВСЁ РАВНО доходит до `ready`
//!
//! Пропадает только `audio.m4a`. Сведение делает
//! [`super::ffmpeg::mix_tracks_in_memory`], потому что по проводу уже идёт
//! PCM16 16 кГц моно — ровно то, что принимает whisper. Это и есть «ffmpeg
//! убран с критического пути расшифровки», и на это есть тест.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::ffmpeg::{self, MixTrack, TrackOrigin};
use super::pertrack;
use super::proc::ProcError;
use super::render::{self, TranscriptMeta};
use super::speakers::{self, AttributedSegment, Interval, SpeakerConfig, SpeechEvent};
use super::whisper::{self, WhisperRequest};
use crate::db::repo::{self, segments::NewSegment, spans::NewSpan};
use crate::error::MeetError;
use crate::ingest::{journal::EventJournal, Manifest};
use crate::state::MeetState;
use crate::status::MeetStatus;
use crate::ws::protocol::{MeetServerMsg, Phase};

/// Итог прогона — то, что уходит в лог и в тесты.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineReport {
    pub meeting_id: String,
    pub tracks: usize,
    pub segments: usize,
    pub speakers: usize,
    /// Архивный файл. `None` — ffmpeg не было.
    pub audio_path: Option<PathBuf>,
    pub transcript_path: PathBuf,
    /// Применённая коррекция дрейфа.
    pub drift_scale: Option<f64>,
    /// Ждал ли прогон очереди whisper.
    pub queued: bool,
    /// Сколько заняла расшифровка.
    pub whisper_ms: u128,
    /// Сколько дорожек расшифровано ПО ОТДЕЛЬНОСТИ. `0` — расшифровывали микс.
    pub per_track: usize,
    /// Сколько МИЛЛИСЕКУНД ЗВУКА реально уехало в whisper.
    ///
    /// Существует ради обоснования пер-трекового пути: «стоит примерно
    /// столько же, сколько микс» — это утверждение о количестве звука, и его
    /// надо уметь измерить, а не предполагать. На миксовом пути равно
    /// длительности встречи, на пер-трековом — сумме вырезанной речи по всем
    /// дорожкам.
    pub audio_sent_ms: i64,
    /// Чем кончилось саммари. `None` — оно выключено конфигом.
    ///
    /// **Провал здесь не делает провалом весь прогон**: `Err` возвращается
    /// только когда нет транскрипта, а саммари считается поверх готового.
    pub summary: Option<crate::status::SummaryStatus>,
}

/// Этап, с которого начинается прогон.
///
/// Существует ради `POST /api/meet/meetings/:id/retry?from=`: строки этого
/// перечисления — те самые значения параметра `from`, и других не бывает.
/// Единственный источник их написания, чтобы роут, статус-машина и фронт не
/// разъехались на строковом литерале.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Сведение дорожек, архивный файл, проверка дрейфа — и дальше всё.
    Transcoding,
    /// Расшифровка и всё, что после неё. Сведение всё равно выполняется:
    /// `work/mix.wav` — транзитный файл, его чистят после успешного прогона, и
    /// whisper нечего было бы читать.
    Transcribing,
    /// Только саммари поверх готового транскрипта.
    Summarizing,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Transcoding, Stage::Transcribing, Stage::Summarizing];

    pub fn as_str(self) -> &'static str {
        self.status().as_str()
    }

    /// Разбор параметра `from`. `None` — этап неизвестен.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// Статус, в который встреча переводится перед прогоном этого этапа.
    pub fn status(self) -> MeetStatus {
        match self {
            Self::Transcoding => MeetStatus::Transcoding,
            Self::Transcribing => MeetStatus::Transcribing,
            Self::Summarizing => MeetStatus::Summarizing,
        }
    }
}

/// Ставит пост-обработку встречи в фон.
///
/// Возвращается немедленно: пост-обработка часовой встречи идёт минуты, и
/// держать на ней HTTP-запрос или воркер записи незачем.
pub fn spawn(state: &Arc<MeetState>, meeting_id: &str) {
    spawn_from(state, meeting_id, Stage::Transcoding);
}

/// То же с указанием этапа — путь `retry?from=`.
pub fn spawn_from(state: &Arc<MeetState>, meeting_id: &str, stage: Stage) {
    let worker_state = state.clone();
    let id = meeting_id.to_string();
    tokio::spawn(async move {
        let register = worker_state.clone();
        let handle = tokio::spawn(async move {
            let outcome = run_from(&worker_state, &id, stage).await;
            // Подчистка ПОСЛЕ КАЖДОЙ ВСТРЕЧИ, а не только по расписанию: за
            // полчаса до следующего тика успевают пройти ещё две записи, и
            // упереться в дисковый бюджет можно ровно между ними. Сама
            // `sweep_once` защищает всё, что сейчас пишется и обрабатывается,
            // поэтому вызов отсюда безопасен.
            crate::retention::sweep_once(&worker_state).await;
            match outcome {
                Ok(report) => tracing::info!(
                    target: "forge_meet",
                    meeting_id = %report.meeting_id,
                    segments = report.segments,
                    speakers = report.speakers,
                    queued = report.queued,
                    whisper_ms = report.whisper_ms,
                    "forge-meet: пост-обработка завершена"
                ),
                Err(e) => tracing::warn!(
                    target: "forge_meet",
                    meeting_id = %id,
                    error = %e,
                    "forge-meet: пост-обработка не удалась"
                ),
            }
        });
        register.register_worker(handle).await;
    });
}

/// Полный прогон пост-обработки одной встречи.
pub async fn run(state: &Arc<MeetState>, meeting_id: &str) -> Result<PipelineReport, MeetError> {
    run_from(state, meeting_id, Stage::Transcoding).await
}

/// Прогон с указанного этапа.
pub async fn run_from(
    state: &Arc<MeetState>,
    meeting_id: &str,
    stage: Stage,
) -> Result<PipelineReport, MeetError> {
    let cancel = state.register_job(meeting_id).await;
    let outcome = match stage {
        Stage::Summarizing => summary_only(state, meeting_id, &cancel).await,
        _ => run_inner(state, meeting_id, &cancel, stage).await,
    };
    state.finish_job(meeting_id).await;

    match outcome {
        Ok(report) => Ok(report),
        Err(stage) => {
            let failed = fail_meeting(state, meeting_id, &stage).await;
            Err(failed)
        }
    }
}

/// Повтор ОДНОГО саммари поверх готового транскрипта.
///
/// Отдельная ветка, а не флаг внутри общего прогона: у неё нет ни сведения, ни
/// whisper, ни очереди GPU — общего с пайплайном здесь только каталог встречи.
/// Отчёт собирается из базы, потому что считать заново нечего.
///
/// **Провалиться она не может по построению**: `summary::summarize` доводит
/// встречу до `ready` при любом исходе, а `Err` этой функции означал бы
/// `failed`, то есть потерю транскрипта — ровно то, что правило 5 запрещает.
async fn summary_only(
    state: &Arc<MeetState>,
    meeting_id: &str,
    cancel: &CancellationToken,
) -> Result<PipelineReport, StageFailure> {
    let meeting = repo::meetings::get(state, meeting_id)
        .await
        .map_err(|e| StageFailure::internal("resolve", e.to_string()))?;

    progress(state, meeting_id, Phase::Summarizing, None);
    let outcome = crate::summary::summarize(state, meeting_id, cancel).await;

    let segments = repo::segments::count(state, meeting_id).await.unwrap_or(0);
    let speakers = repo::segments::speaker_names(state, meeting_id)
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    Ok(PipelineReport {
        meeting_id: meeting_id.to_string(),
        tracks: 0,
        segments,
        speakers,
        audio_path: meeting.audio_path.as_ref().map(PathBuf::from),
        transcript_path: meeting
            .transcript_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        drift_scale: None,
        queued: false,
        whisper_ms: 0,
        per_track: 0,
        audio_sent_ms: 0,
        summary: Some(outcome.status),
    })
}

/// Ошибка этапа вместе с тем, что показать пользователю.
#[derive(Debug, Clone)]
struct StageFailure {
    stage: &'static str,
    error: ProcError,
}

impl StageFailure {
    fn new(stage: &'static str, error: ProcError) -> Self {
        Self { stage, error }
    }

    fn internal(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            error: ProcError::new("internal", message, ""),
        }
    }
}

/// Переводит встречу в `failed` и ДОНОСИТ ХВОСТ STDERR ДО ПОЛЬЗОВАТЕЛЯ.
///
/// В журнал встречи (`meeting_events`) и в кадр `error` — а не только в лог
/// сервера. Пользователю, у которого ffmpeg упал на битом WAV, нужна
/// последняя строка его вывода; без неё ему остаётся только писать в
/// поддержку.
async fn fail_meeting(
    state: &Arc<MeetState>,
    meeting_id: &str,
    failure: &StageFailure,
) -> MeetError {
    let full = failure.error.full_message();
    let _ = repo::events::note(
        state,
        meeting_id,
        MeetStatus::Failed.as_str(),
        failure.error.code,
        Some(&full),
    )
    .await;

    // Отмена — не провал: встречу уже перевели в `cancelled`, и трогать
    // статус нельзя.
    if failure.error.code != "cancelled" {
        let _ = repo::meetings::set_status(
            state,
            meeting_id,
            MeetStatus::Failed,
            Some(failure.error.code),
        )
        .await;
    }

    state.broadcast_event(MeetServerMsg::Error {
        meeting_id: Some(meeting_id.to_string()),
        code: failure.error.code.to_string(),
        message: full.clone(),
    });
    tracing::warn!(
        target: "forge_meet",
        meeting_id = %meeting_id,
        stage = failure.stage,
        code = failure.error.code,
        "forge-meet: этап пост-обработки провалился: {full}"
    );
    MeetError::from(failure.error.clone())
}

async fn run_inner(
    state: &Arc<MeetState>,
    meeting_id: &str,
    cancel: &CancellationToken,
    stage: Stage,
) -> Result<PipelineReport, StageFailure> {
    // ЖДЁМ ПРОБ ОКРУЖЕНИЯ. Они идут отдельной задачей и на холодной машине
    // занимают секунды (`ggml_metal_library_init` — 12,5 с при первом в жизни
    // запуске). Стартовать без них нельзя: пайплайн решал бы, что ffmpeg и
    // whisper отсутствуют, и писал бы `failed` там, где надо было подождать.
    //
    // Ждать приходится и на пути `reconcile_on_boot`: он идёт ПЕРВЫМ делом
    // после базы, то есть заведомо раньше окончания проб.
    wait_for_probes(state, cancel).await?;

    let meeting = repo::meetings::get(state, meeting_id)
        .await
        .map_err(|e| StageFailure::internal("resolve", e.to_string()))?;
    let dir = PathBuf::from(&meeting.meeting_dir);
    let manifest = Manifest::load(&dir);

    // Манифеста может не быть (запись прервали до первого чекпоинта) — тогда
    // читаем каталог как есть. Каталог самодостаточен намеренно.
    let sample_rate = manifest
        .as_ref()
        .map(|m| m.sample_rate)
        .unwrap_or(state.config.audio_sample_rate);
    let timeline_ms = manifest.as_ref().map(|m| m.duration_ms as i64).unwrap_or(0);

    let deps = state.deps_snapshot().await;
    let cfg = &state.config;
    let ffmpeg_timeout = std::time::Duration::from_secs(cfg.ffmpeg_timeout_secs);

    // --- этап 1: сведение --------------------------------------------------
    progress(state, meeting_id, Phase::Transcoding, None);

    let raw_dir = dir.join(crate::ingest::recorder::RAW_DIR);
    let tracks = match &manifest {
        Some(m) => ffmpeg::mix_tracks(&m.tracks, &raw_dir, TrackOrigin::SharedT0),
        None => scan_raw_tracks(&raw_dir),
    };
    if tracks.is_empty() {
        return Err(StageFailure::internal(
            "mixdown",
            "в каталоге записи нет ни одной дорожки",
        ));
    }

    let mix_path = dir.join(super::WORK_DIR).join(super::MIX_WAV);
    let ffmpeg_bin = deps.ffmpeg.path.as_ref().map(PathBuf::from);
    match &ffmpeg_bin {
        Some(bin) => {
            ffmpeg::mixdown_with_ffmpeg(bin, &tracks, &mix_path, sample_rate, ffmpeg_timeout, cancel)
                .await
                .map_err(|e| StageFailure::new("mixdown", e))?;
        }
        None => {
            // Честная деградация: фильтров нет, транскрипт есть.
            ffmpeg::mix_tracks_in_memory(&tracks, sample_rate, &mix_path).map_err(|e| {
                StageFailure::internal("mixdown", format!("сведение без ffmpeg не удалось: {e}"))
            })?;
            let _ = repo::events::note(
                state,
                meeting_id,
                MeetStatus::Transcoding.as_str(),
                "ffmpeg_missing",
                Some("архивного файла не будет, дорожки сведены своими силами"),
            )
            .await;
        }
    }

    // --- этап 2: архивный файл (только с ffmpeg) ---------------------------
    //
    // Повтор ИМЕННО расшифровки (`retry?from=transcribing`) архив не
    // пересобирает: он уже лежит на диске, побайтово тот же, и минута
    // перекодирования часа звука не приблизила бы новый транскрипт ни на шаг.
    let mut audio_path = None;
    let archive_exists = dir.join(super::ARCHIVE_AUDIO).is_file();
    let skip_archive = stage >= Stage::Transcribing && archive_exists;
    if skip_archive {
        audio_path = Some(dir.join(super::ARCHIVE_AUDIO));
    }
    if let (Some(bin), false) = (&ffmpeg_bin, skip_archive) {
        let archive = dir.join(super::ARCHIVE_AUDIO);
        match ffmpeg::encode_archive(bin, &mix_path, &archive, ffmpeg_timeout, cancel).await {
            Ok(()) => audio_path = Some(archive),
            Err(e) if e.code == "cancelled" => return Err(StageFailure::new("archive", e)),
            Err(e) => {
                // Архив — приятная, но не обязательная часть: без него
                // остаются и расшифровка, и пер-трековое аудио. Ронять из-за
                // него встречу нельзя.
                let _ = repo::events::note(
                    state,
                    meeting_id,
                    MeetStatus::Transcoding.as_str(),
                    "archive_failed",
                    Some(&e.full_message()),
                )
                .await;
                tracing::warn!(
                    target: "forge_meet",
                    meeting_id = %meeting_id,
                    "forge-meet: архивный файл не собрался: {}",
                    e.full_message()
                );
            }
        }
    }

    // --- этап 3: дрейф таймлайна -------------------------------------------
    let actual_ms = match deps.ffprobe.path.as_ref() {
        Some(p) => {
            ffmpeg::probe_duration_ms(Path::new(p), &mix_path, ffmpeg_timeout).await
        }
        None => None,
    };
    let drift_scale = match actual_ms {
        Some(actual) => {
            let k = speakers::drift_scale(timeline_ms, actual as i64, cfg.drift_threshold_ms as i64);
            if let Some(k) = k {
                let _ = repo::events::note(
                    state,
                    meeting_id,
                    MeetStatus::Transcoding.as_str(),
                    "drift_corrected",
                    Some(&format!(
                        "таймлайн {timeline_ms} мс, аудио {actual} мс, коэффициент {k:.6}"
                    )),
                )
                .await;
            }
            k
        }
        None => {
            // Без ffprobe коррекция пропускается — это НЕ ошибка, но факт
            // обязан быть виден: иначе через месяц не понять, почему
            // таймкоды слегка плывут.
            let _ = repo::events::note(
                state,
                meeting_id,
                MeetStatus::Transcoding.as_str(),
                "drift_check_skipped",
                Some("ffprobe недоступен, длительность аудио не проверена"),
            )
            .await;
            None
        }
    };

    // След пост-обработки уезжает В МАНИФЕСТ: каталог встречи обязан уметь
    // объяснить свои таймкоды без базы.
    if let Some(mut m) = manifest.clone() {
        m.postprocess = Some(crate::ingest::manifest::PostprocessInfo {
            drift_checked: actual_ms.is_some(),
            drift_scale,
            actual_duration_ms: actual_ms,
            archive: audio_path.is_some(),
            // Заполняется ПОСЛЕ расшифровки: сейчас ещё неизвестно, каким
            // путём она пойдёт.
            per_track: 0,
        });
        if let Err(e) = m.save(&dir) {
            tracing::warn!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                error = %e,
                "forge-meet: манифест не обновился после сведения"
            );
        }
    }

    // --- этап 4: таймлайн речи ---------------------------------------------
    //
    // Разбирается ДО расшифровки, а не после неё, и это не перестановка ради
    // красоты: от таймлайна зависит, ЧТО именно уедет в whisper. В режиме
    // `PerParticipant` расшифровывается не микс, а вырезанная речь каждой
    // дорожки по отдельности — решать это, уже потратив GPU-время на микс,
    // было бы поздно.
    //
    // КОРРЕКЦИЯ ПРИМЕНЯЕТСЯ К ИНТЕРВАЛАМ, А НЕ К СЕГМЕНТАМ, и это не деталь
    // реализации. Две оси времени живут в разных местах:
    //
    // * сегменты whisper — на оси АУДИО: он читает `work/mix.wav`, и его
    //   таймкоды по построению совпадают с тем, что услышит человек в плеере;
    // * интервалы речи — на оси СОБЫТИЙ: их метки ставит страница по
    //   `performance.now()`.
    //
    // Расходятся именно эти две оси, и приводить надо ту, что не совпадает со
    // звуком. Скорректируй мы сегменты — таймкоды в расшифровке разъехались
    // бы с плеером, то есть клик по реплике попадал бы мимо неё; это ровно
    // тот случай, когда «исправление» дрейфа его создаёт.
    //
    // По той же причине НАРЕЗКА тоже идёт по скорректированным интервалам:
    // резать файл, который живёт на оси аудио, по меткам с оси событий значит
    // отрезать речь на величину дрейфа.
    let timeline = timeline_from_journal(
        &dir,
        timeline_ms.max(0),
        &crate::bot::attrib::AttribConfig::from(cfg),
    );
    let mut intervals = timeline.intervals.clone();
    if let Some(k) = drift_scale {
        speakers::remap_intervals(&mut intervals, k);
    }
    tracing::info!(
        target: "forge_meet",
        meeting_id = %meeting_id,
        stage = %timeline.binding.stage,
        source = %timeline.binding.source,
        named = timeline.binding.named_tracks,
        tracks = timeline.binding.tracks,
        confidence = timeline.binding.confidence,
        "forge-meet: каскад привязки имён"
    );
    // След каскада — В МАНИФЕСТ: без него нельзя понять, доверять именам или
    // нет. «Алиса» из `msid` это факт, «Алиса» из корреляции с уверенностью
    // 0,71 — гипотеза.
    if let Some(mut m) = Manifest::load(&dir) {
        m.speakers = Some(timeline.binding.clone());
        if let Err(e) = m.save(&dir) {
            tracing::warn!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                error = %e,
                "forge-meet: манифест не принял след каскада"
            );
        }
    }
    // Участники — в базу, с ФАКТИЧЕСКИ сработавшей ступенью в колонке
    // `source`: интерфейс показывает «Участник 2» и имя из DOM по-разному, а
    // без ступени отличить их было бы нечем.
    for b in &timeline.report.bindings {
        let key = b
            .key
            .clone()
            .unwrap_or_else(|| format!("track-{}", b.idx));
        if let Err(e) = repo::participants::upsert(
            state,
            meeting_id,
            &key,
            b.name.as_deref(),
            Some(b.idx as i64),
            Some(b.stage.as_str()),
            Some(b.confidence),
        )
        .await
        {
            tracing::warn!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                error = %e,
                "forge-meet: участник не записался"
            );
        }
    }

    // --- этап 5: расшифровка -----------------------------------------------
    //
    // Статус двигаем, только если встреча ещё не там: при `retry?from=` его
    // уже поставил роут, и `transcribing → transcribing` таблица переходов
    // законно отвергает.
    if meeting.status != MeetStatus::Transcribing {
        repo::meetings::set_status(
            state,
            meeting_id,
            MeetStatus::Transcribing,
            Some("transcoded"),
        )
        .await
        .map_err(|e| StageFailure::internal("status", e.to_string()))?;
    }

    // **РАЗВИЛКА СТАДИИ: облако или локальный whisper.**
    //
    // В облачном режиме отсюда и до конца этапа исчезают ТРИ вещи, и все три —
    // не по недосмотру, а потому что их причина отпала вместе с локальным GPU:
    //
    // 1. проверка `whisper-cli` + весов + Metal: облаку не нужно ничего из
    //    этого, и отказ «модели нет на диске» означал бы отказ по требованию,
    //    которого никто не предъявлял;
    // 2. ожидание конца всех записей: гейт существует потому, что Chrome
    //    рендерит встречу на том же Metal, что и whisper. Облако с Chrome не
    //    конкурирует вообще;
    // 3. семафор «whisper строго один»: он держит единственный GPU. Облачные
    //    задания идут по сети параллельно, и их сериализация означала бы, что
    //    вторая встреча ждёт первую БЕЗ ЕДИНОЙ ПРИЧИНЫ.
    let cloud_stt = cfg.stt_backend.is_cloud();
    if let Some(code) = deps.transcribe_blocker_for(cfg.stt_backend, cfg.cloud_configured()) {
        return Err(StageFailure::new(
            "whisper",
            ProcError::new(code, format!("расшифровка недоступна: {code}"), ""),
        ));
    }

    // Слот и гейт — только у локального пути (см. развилку выше).
    let slot = if cloud_stt {
        None
    } else {
        // ЖДЁМ конца всех записей: Chrome и whisper не делят GPU.
        wait_until_no_recordings(state, meeting_id, cancel).await?;

        let slot = state
            .hub
            .acquire_whisper_queued()
            .await
            .map_err(|e| StageFailure::internal("queue", e.to_string()))?;
        if slot.was_queued() {
            state.broadcast_event(MeetServerMsg::Progress {
                meeting_id: meeting_id.to_string(),
                phase: Phase::Transcribing,
                done: None,
                total: None,
                queued: true,
                queue_position: Some(slot.queue_position()),
            });
        }
        Some(slot)
    };
    // Пока стояли в очереди, могли отменить.
    if cancel.is_cancelled() {
        return Err(StageFailure::new(
            "whisper",
            ProcError::new("cancelled", "операция отменена", ""),
        ));
    }

    // Путь к бинарю ищется только там, где он нужен: в облачном режиме его
    // может не быть на машине вовсе, и `ok_or_else` ниже отказал бы напрасно.
    let bin = if cloud_stt {
        None
    } else {
        Some(whisper::binary_from(&deps).ok_or_else(|| {
            StageFailure::internal("whisper", "путь к whisper потерялся между пробой и запуском")
        })?)
    };
    let opts = whisper::RunOptions {
        timeout: std::time::Duration::from_secs(cfg.whisper_timeout_secs),
        metal_grace: std::time::Duration::from_secs(cfg.metal_grace_secs),
        cancel: cancel.clone(),
    };

    let reporter = state.clone();
    let progress_id = meeting_id.to_string();
    let on_progress = move |p: u8| {
        reporter.broadcast_event(MeetServerMsg::Progress {
            meeting_id: progress_id.clone(),
            phase: Phase::Transcribing,
            done: Some(u64::from(p)),
            total: Some(100),
            queued: false,
            queue_position: None,
        });
    };

    // Путь расшифровки выбирается ЗДЕСЬ, под уже взятым единственным слотом
    // очереди: с точки зрения остальных встреч пер-трековый прогон — это один
    // прогон, просто из нескольких файлов.
    let jobs = per_track_jobs(&timeline, &raw_dir, drift_scale);
    let mix_ms = timeline_ms.max(actual_ms.map(|v| v as i64).unwrap_or(0));
    // Пер-трековый путь есть у ОБОИХ движков. Раньше облаку он был закрыт
    // наглухо («N загрузок вместо одной = N-кратные деньги»), и это оказалось
    // неверно: в облако уезжает не запись, а вырезанная речь — 14 % от микса
    // на живом замере. Разница между движками осталась ровно одна — чем
    // меряется бюджет (см. `per_track_decision`).
    let decision = per_track_decision(cfg, manifest.as_ref(), &jobs, mix_ms, cloud_stt);
    if let Err(why) = decision {
        // Событие пишется, только когда режим ЗАЯВЛЯЛ дорожку на человека, а
        // пути всё равно не случилось: на Телемосте этот отказ штатный и в
        // журнале был бы шумом на каждой встрече.
        if manifest
            .as_ref()
            .and_then(|m| m.track_mode)
            .is_some_and(|m| m.tracks_are_people())
        {
            let _ = repo::events::note(
                state,
                meeting_id,
                MeetStatus::Transcribing.as_str(),
                "per_track_skipped",
                Some(why),
            )
            .await;
        }
        tracing::debug!(
            target: "forge_meet",
            meeting_id = %meeting_id,
            "forge-meet: расшифровка по миксу ({why})"
        );
    }

    let started = std::time::Instant::now();
    let (segments, per_track, audio_sent_ms) = if cloud_stt {
        // --- облачная расшифровка -------------------------------------------
        //
        // Отказ облака — это отказ ЭТОГО этапа и ничего больше: WAV дорожек,
        // микс и манифест уже на диске и остаются там. Правило «`failed`
        // только когда нет транскрипта» держится тем, что сюда мы попадаем
        // после записи, а не вместо неё.
        let client = crate::cloud::CloudClient::new(&cfg.cloud())
            .map_err(|e| StageFailure::new("cloud", ProcError::from(e)))?;
        let ffmpeg = ffmpeg_bin.clone().ok_or_else(|| {
            // Единственная внешняя зависимость облачного пути: окно нужно
            // вырезать и сжать в mp3 (сырой WAV роняет их шлюз).
            StageFailure::new(
                "cloud",
                ProcError::new(
                    "ffmpeg_unavailable",
                    "облачная расшифровка режет запись на окна через ffmpeg, а его нет",
                    "",
                ),
            )
        })?;
        let cloud_cfg = cfg.cloud();
        let stt_opts = crate::cloud::stt::SttOptions {
            ffmpeg,
            ffmpeg_timeout,
            window_ms: cloud_cfg.window_ms(),
            model: cloud_cfg.stt_model.clone(),
            language: cloud_cfg.stt_language.clone(),
            request_timeout: std::time::Duration::from_secs(cloud_cfg.stt_timeout_secs),
            cancel: cancel.clone(),
            // Речь НА ОБЩЕЙ ОСИ — по ней границы окон уезжают в тишину.
            // Реплика, разрезанная посередине, задваивается: модель
            // договаривает оборванную фразу, а следующее окно расшифровывает
            // её настоящий хвост (`forge-12532`, измерено живьём).
            speech: intervals.iter().map(|i| (i.start_ms, i.end_ms)).collect(),
        };
        let speaker_cfg = SpeakerConfig::from(cfg);
        match decision {
            Ok(()) => {
                // --- облако ПО ДОРОЖКАМ ---------------------------------
                //
                // В облако уходит не запись, а вырезанная речь каждой
                // дорожки: дешевле микса, а подпись спикера становится
                // фактом вместо перекрытия интервалов.
                let req = crate::cloud::stt::CloudTrackRequest {
                    work_dir: dir.join(super::WORK_DIR),
                    sample_rate,
                    total_ms: mix_ms,
                    pad_ms: pertrack::CUT_PAD_MS,
                };
                let outcome = crate::cloud::stt::transcribe_tracks(
                    &client,
                    &jobs,
                    &req,
                    &stt_opts,
                    Some(&on_progress),
                )
                .await
                .map_err(|e| StageFailure::new("cloud_stt", e))?;

                // Дыра в расшифровке — не молчание, и здесь она ещё и
                // адресная: человек обязан узнать, в ЧЬЕЙ дорожке пропуск.
                for (event, note) in [
                    ("cloud_stt_gaps", outcome.gap_note()),
                    ("cloud_stt_forced_cuts", outcome.forced_note()),
                ] {
                    let Some(note) = note else { continue };
                    let _ = repo::events::note(
                        state,
                        meeting_id,
                        MeetStatus::Transcribing.as_str(),
                        event,
                        Some(&note),
                    )
                    .await;
                    tracing::warn!(
                        target: "forge_meet",
                        meeting_id = %meeting_id,
                        "forge-meet: {note}"
                    );
                }

                let sent = outcome.speech_ms();
                (
                    pertrack::merge_tracks(&outcome.tracks, &intervals, &speaker_cfg),
                    outcome.tracks.len(),
                    sent,
                )
            }
            Err(_) => {
                // --- облако ПО МИКСУ ------------------------------------
                let outcome = crate::cloud::stt::transcribe_file(
                    &client,
                    &mix_path,
                    mix_ms,
                    &dir.join(super::WORK_DIR),
                    &stt_opts,
                    Some(&on_progress),
                )
                .await
                .map_err(|e| StageFailure::new("cloud_stt", e))?;

                // Дыра в расшифровке — не молчание: часть окон могла не
                // доехать, и человек обязан узнать, ГДЕ именно транскрипт
                // неполон. Граница окна, поставленная внутрь непрерывной
                // речи, называется там же: на ней возможен задвоенный хвост.
                for (event, note) in [
                    ("cloud_stt_gaps", outcome.gap_note()),
                    ("cloud_stt_forced_cuts", outcome.forced_note()),
                ] {
                    let Some(note) = note else { continue };
                    let _ = repo::events::note(
                        state,
                        meeting_id,
                        MeetStatus::Transcribing.as_str(),
                        event,
                        Some(&note),
                    )
                    .await;
                    tracing::warn!(
                        target: "forge_meet",
                        meeting_id = %meeting_id,
                        "forge-meet: {note}"
                    );
                }
                (
                    speakers::attribute_segments(&outcome.segments, &intervals, &speaker_cfg),
                    0,
                    mix_ms,
                )
            }
        }
    } else {
        match decision {
        Ok(()) => {
            let bin = bin.clone().expect("локальный путь без бинаря whisper");
            let req = pertrack::PerTrackRequest {
                bin,
                model: whisper::model_path(cfg),
                work_dir: dir.join(super::WORK_DIR),
                language: cfg.whisper_language.clone(),
                sample_rate,
                total_ms: mix_ms,
                pad_ms: pertrack::CUT_PAD_MS,
                flags: &deps.whisper_flags,
                opts: opts.clone(),
            };
            let tracks = pertrack::transcribe_tracks(&jobs, &req, Some(&on_progress))
                .await
                .map_err(|e| StageFailure::new("whisper", e))?;
            let sent: i64 = tracks.iter().map(|t| t.cut_ms).sum();
            let speaker_cfg = SpeakerConfig::from(cfg);
            (
                pertrack::merge_tracks(&tracks, &intervals, &speaker_cfg),
                tracks.len(),
                sent,
            )
        }
        Err(_) => {
            let bin = bin.clone().expect("локальный путь без бинаря whisper");
            let req = WhisperRequest {
                bin,
                model: whisper::model_path(cfg),
                wav: mix_path.clone(),
                out_prefix: whisper::out_prefix(&dir),
                language: cfg.whisper_language.clone(),
                beam_size: 5,
                vad_model: None,
            };
            let outcome = whisper::run(&req, &deps.whisper_flags, &opts, Some(&on_progress))
                .await
                .map_err(|e| StageFailure::new("whisper", e))?;
            let speaker_cfg = SpeakerConfig::from(cfg);
            (
                speakers::attribute_segments(&outcome.segments, &intervals, &speaker_cfg),
                0,
                mix_ms,
            )
        }
        }
    };
    let whisper_ms = started.elapsed().as_millis();
    let queued = slot.as_ref().is_some_and(|s| s.was_queued());
    drop(slot);

    // --- этап 6: атрибуция и рендер ----------------------------------------
    let mut attributed = segments;
    // Чем получены таймкоды — В МАНИФЕСТ, рядом со следом каскада: по самим
    // таймкодам это неотличимо, а разница принципиальна (факт против оценки).
    if per_track > 0 {
        if let Some(mut m) = Manifest::load(&dir) {
            if let Some(p) = m.postprocess.as_mut() {
                p.per_track = per_track;
            }
            let _ = m.save(&dir);
        }
    }

    let meta = TranscriptMeta {
        meeting_id: meeting_id.to_string(),
        platform: meeting.platform.as_str().to_string(),
        url: meeting.canonical_url.clone(),
        title: meeting.title.clone(),
        started_at_unix_ms: manifest.as_ref().map(|m| m.t0_unix_ms),
        duration_ms: actual_ms.map(|v| v as i64).unwrap_or(timeline_ms),
        // ШАПКА НАЗЫВАЕТ ТОТ ПУТЬ, КОТОРЫЙ ОТРАБОТАЛ, а не тот, что настроен
        // для локального движка. Раньше здесь безусловно стояли
        // `whisper_model`/`whisper_language`, и облачная расшифровка
        // подписывалась именем локальной модели — по артефакту нельзя было
        // сказать, каким путём он получен.
        engine: if cloud_stt {
            render::ENGINE_CLOUD.to_string()
        } else {
            render::ENGINE_LOCAL.to_string()
        },
        model: if cloud_stt {
            cfg.cloud().stt_model.clone()
        } else {
            cfg.whisper_model.clone()
        },
        language: if cloud_stt {
            cfg.cloud().stt_language.clone()
        } else {
            cfg.whisper_language.clone()
        },
        drift_scale,
    };
    let (transcript_path, _md_path) = render::write_transcript(&dir, &meta, &attributed)
        .map_err(|e| StageFailure::internal("render", format!("транскрипт не записался: {e}")))?;

    // --- этап 7: база ------------------------------------------------------
    //
    // Номер дорожки в базе заполняется ТОЛЬКО на пер-трековом пути: там он
    // факт (сегмент пришёл из `raw/track-N.wav`). На миксовом писать его было
    // бы враньём — сегмент не принадлежит ни одной дорожке, он принадлежит
    // сумме всех.
    let track_of: HashMap<String, i64> = if per_track > 0 {
        jobs.iter()
            .map(|j| (j.label.clone(), j.idx as i64))
            .collect()
    } else {
        HashMap::new()
    };
    store_results(state, meeting_id, &intervals, &attributed, &track_of)
        .await
        .map_err(|e| StageFailure::internal("store", e.to_string()))?;

    repo::meetings::set_paths(
        state,
        meeting_id,
        audio_path.as_ref().map(|p| p.display().to_string()).as_deref(),
        Some(&transcript_path.display().to_string()),
        None,
    )
    .await
    .map_err(|e| StageFailure::internal("store", e.to_string()))?;

    // Транзитные файлы больше не нужны: всё, что из них следовало, лежит в
    // `transcript.json` и в базе. Пер-трековые WAV в `raw/` при этом
    // ОСТАЮТСЯ — по ним переслушивают спикеров и переигрывают расшифровку.
    let _ = std::fs::remove_dir_all(dir.join(super::WORK_DIR));

    let mut report = PipelineReport {
        meeting_id: meeting_id.to_string(),
        tracks: tracks.len(),
        segments: attributed.len(),
        speakers: render::speaker_order(&attributed).len(),
        audio_path,
        transcript_path,
        drift_scale,
        queued,
        whisper_ms,
        per_track,
        audio_sent_ms,
        summary: None,
    };

    // --- этап 7: саммари ---------------------------------------------------
    //
    // ГЛАВНОЕ ПРАВИЛО: отсюда и до конца встреча УЖЕ НЕ МОЖЕТ УПАСТЬ. Транскрипт
    // на диске, в базе и в кадре `transcript_delta`; всё, что осталось, — это
    // текст поверх него. Поэтому саммари не возвращает `StageFailure`, а
    // доводит встречу до `ready` само, записывая свою неудачу в СВОЙ статус.
    report.summary = Some(summarize_step(state, meeting_id, cancel).await?);

    // `attributed` дальше не нужен, но пусть компилятор видит владение явно:
    // случайное переиспользование после `store_results` означало бы, что в
    // базе и в файле разные данные.
    attributed.clear();
    Ok(report)
}

/// Саммари в конце пайплайна: `transcribing → summarizing → ready`.
///
/// `Err` возвращается ровно в одном случае — когда не удалось записать сам
/// статус, то есть когда сломана база. Провал Claude сюда не попадает никогда:
/// у него свой статус.
async fn summarize_step(
    state: &Arc<MeetState>,
    meeting_id: &str,
    cancel: &CancellationToken,
) -> Result<crate::status::SummaryStatus, StageFailure> {
    use crate::status::SummaryStatus;

    if !state.config.summary_enabled {
        // Саммари выключено — путь `transcribing → ready` объявлен в таблице
        // переходов именно для этого.
        repo::meetings::set_status(state, meeting_id, MeetStatus::Ready, Some("transcribed"))
            .await
            .map_err(|e| StageFailure::internal("status", e.to_string()))?;
        return Ok(SummaryStatus::None);
    }

    if let Err(e) =
        repo::meetings::set_status(state, meeting_id, MeetStatus::Summarizing, Some("transcribed"))
            .await
    {
        // Перевести в `summarizing` не вышло — саммари не будет, но транскрипт
        // есть, и встреча обязана дойти до `ready` всё равно.
        tracing::warn!(
            target: "forge_meet",
            meeting_id = %meeting_id,
            error = %e,
            "forge-meet: не удалось начать саммари, встреча идёт в ready без него"
        );
        repo::meetings::set_status(state, meeting_id, MeetStatus::Ready, Some("transcribed"))
            .await
            .map_err(|e| StageFailure::internal("status", e.to_string()))?;
        return Ok(SummaryStatus::None);
    }
    repo::meetings::set_summary_status(state, meeting_id, SummaryStatus::Pending)
        .await
        .ok();
    progress(state, meeting_id, Phase::Summarizing, None);

    let outcome = crate::summary::summarize(state, meeting_id, cancel).await;
    Ok(outcome.status)
}

/// Ждёт окончания проб внешних бинарей.
///
/// Без ограничения по времени, но с оглядкой на отмену: непроверенное
/// окружение — это не «ничего нет», а «ещё не смотрели», и разница между
/// ними стоит целой встречи.
async fn wait_for_probes(
    state: &Arc<MeetState>,
    cancel: &CancellationToken,
) -> Result<(), StageFailure> {
    let poll = std::time::Duration::from_millis(state.config.whisper_gate_poll_ms.max(10));
    loop {
        if state.deps_snapshot().await.probed {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Err(StageFailure::new(
                "probe",
                ProcError::new("cancelled", "операция отменена", ""),
            ));
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = cancel.cancelled() => {}
        }
    }
}

/// Ждёт, пока не закончится ПОСЛЕДНЯЯ активная запись.
///
/// Chrome рендерит встречу на том же GPU, и запуск whisper во время записи
/// испортит звук именно у неё. Ждём столько, сколько нужно: запись уже на
/// диске, а испорченный звук не восстановить.
async fn wait_until_no_recordings(
    state: &Arc<MeetState>,
    meeting_id: &str,
    cancel: &CancellationToken,
) -> Result<(), StageFailure> {
    let poll = std::time::Duration::from_millis(state.config.whisper_gate_poll_ms.max(10));
    let mut announced = false;
    loop {
        if cancel.is_cancelled() {
            return Err(StageFailure::new(
                "gate",
                ProcError::new("cancelled", "операция отменена", ""),
            ));
        }
        let active = state.hub.active_meetings().await;
        if active.is_empty() {
            return Ok(());
        }
        if !announced {
            announced = true;
            tracing::info!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                active = active.len(),
                "forge-meet: whisper ждёт окончания записи — GPU занят браузером"
            );
            // Пользователь обязан видеть, что это ожидание, а не зависание.
            state.broadcast_event(MeetServerMsg::Progress {
                meeting_id: meeting_id.to_string(),
                phase: Phase::Transcribing,
                done: None,
                total: None,
                queued: true,
                queue_position: None,
            });
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = cancel.cancelled() => {}
        }
    }
}

fn progress(state: &Arc<MeetState>, meeting_id: &str, phase: Phase, done: Option<u64>) {
    state.broadcast_event(MeetServerMsg::Progress {
        meeting_id: meeting_id.to_string(),
        phase,
        done,
        total: None,
        queued: false,
        queue_position: None,
    });
}

/// Дорожки по содержимому каталога — на случай пропавшего манифеста.
fn scan_raw_tracks(raw_dir: &Path) -> Vec<MixTrack> {
    let Ok(entries) = std::fs::read_dir(raw_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("wav")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("track-"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| MixTrack { path, delay_ms: 0 })
        .collect()
}

/// Итог разбора журнала: интервалы речи и след каскада привязки имён.
pub struct Timeline {
    /// Интервалы речи с ИМЕНАМИ (или «Участник N»).
    pub intervals: Vec<Interval>,
    /// Что сделал каскад — уезжает в манифест и в карточку встречи.
    pub binding: crate::ingest::manifest::SpeakerBinding,
    /// Привязка дорожек: по ней участники попадают в базу.
    pub report: crate::bot::attrib::AttributionReport,
    /// ПЕР-ТРЕКОВЫЕ интервалы речи, до слияния в общий список.
    ///
    /// Нужны нарезке [`super::pertrack`]: она режет КАЖДЫЙ файл по его
    /// собственному VAD, и слитый список для этого не годится — в нём уже
    /// перемешаны дорожки, и восстановить, какой кусок из какого файла,
    /// нельзя. Пусты в режиме `Slots`: там источник интервалов — DOM.
    pub tracks: Vec<TimelineTrack>,
}

/// Дорожка вместе с подписью, ПОД КОТОРОЙ её речь попала в интервалы.
///
/// Подпись хранится рядом, а не выводится заново, потому что выводится она
/// по-разному: имя из каскада, «Участник N» или «Дорожка N» — в зависимости
/// от того, какая ветка выбора источника сработала. Вычисли её нарезка
/// самостоятельно — она разошлась бы с интервалами при первой же смене ветки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineTrack {
    /// Номер файла `raw/track-N.wav`.
    pub idx: usize,
    /// Подпись спикера — та же, что у интервалов этой дорожки.
    pub label: String,
    /// Интервалы речи на оси СОБЫТИЙ (дрейф ещё не применён).
    pub speech: Vec<crate::bot::attrib::Span>,
}

/// Восстанавливает таймлайн речи из журнала записи.
///
/// Журнал, а не база: каталог встречи самодостаточен, и пост-обработка обязана
/// уметь разобрать его после перезапуска devforge, ничего не помня.
///
/// # ИСТОЧНИК ИНТЕРВАЛОВ ВЫБИРАЕТСЯ, А НЕ СКЛАДЫВАЕТСЯ
///
/// В журнале живой встречи лежат ОБА таймлайна сразу: пер-трековый VAD
/// (`speech_start`/`speech_end`) и DOM-индикатор (`active_speaker`). Сложи их —
/// и одна реплика попадёт в расшифровку дважды: раз под именем дорожки, раз под
/// именем человека. Поэтому решение принимается явно:
///
/// | что есть | источник | почему |
/// |---|---|---|
/// | каскад назвал хотя бы одну дорожку | VAD дорожек | пер-трековый детект точнее: он видит наложение речи, а DOM показывает одного |
/// | имён нет, но есть DOM | DOM | режим `Slots` (Телемост): дорожка не соответствует человеку, резать по ней нечего |
/// | нет ни того, ни другого | VAD с «Дорожка N» | лучше безымянные интервалы, чем никаких |
pub fn timeline_from_journal(
    meeting_dir: &Path,
    end_ms: i64,
    cfg: &crate::bot::attrib::AttribConfig,
) -> Timeline {
    use crate::bot::attrib::{self, Span, SpeakerTimeline, TrackTimeline};

    let entries = EventJournal::read(meeting_dir).unwrap_or_default();
    let mut end = end_ms;

    // Дорожки: индекс и открытые интервалы речи.
    let mut tracks: Vec<TrackTimeline> = Vec::new();
    let mut track_pos: HashMap<String, usize> = HashMap::new();
    let mut open_track: HashMap<String, i64> = HashMap::new();
    // Участники по DOM: интервалы активности.
    let mut speakers_dom: Vec<SpeakerTimeline> = Vec::new();
    let mut speaker_pos: HashMap<String, usize> = HashMap::new();
    let mut open_dom: Option<(String, i64)> = None;
    let mut dom_seen = false;

    let track_of = |data: &serde_json::Value,
                        tracks: &mut Vec<TrackTimeline>,
                        pos: &mut HashMap<String, usize>|
     -> Option<usize> {
        let id = data
            .get("track_id")
            .or_else(|| data.get("t"))
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let idx_hint = data.get("idx").and_then(serde_json::Value::as_u64);
        let slot = *pos.entry(id.clone()).or_insert_with(|| {
            tracks.push(TrackTimeline {
                idx: idx_hint.unwrap_or(tracks.len() as u64) as usize,
                track_id: id.clone(),
                msid: data
                    .get("msid")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                ssrc: data
                    .get("ssrc")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u32),
                speech: Vec::new(),
            });
            tracks.len() - 1
        });
        if let Some(idx) = idx_hint {
            tracks[slot].idx = idx as usize;
        }
        Some(slot)
    };

    for e in &entries {
        let at = e.at_ms as i64;
        end = end.max(at);
        let ts = field_ms(&e.data).unwrap_or(at);
        match e.kind.as_str() {
            "track_added" | "track_ended" => {
                let _ = track_of(&e.data, &mut tracks, &mut track_pos);
            }
            "speech_started" | "speech_start" => {
                if let Some(slot) = track_of(&e.data, &mut tracks, &mut track_pos) {
                    open_track.entry(tracks[slot].track_id.clone()).or_insert(ts);
                }
            }
            "speech_ended" | "speech_end" => {
                if let Some(slot) = track_of(&e.data, &mut tracks, &mut track_pos) {
                    let id = tracks[slot].track_id.clone();
                    if let Some(start) = open_track.remove(&id) {
                        if ts > start {
                            tracks[slot].speech.push(Span::new(start, ts));
                        }
                    }
                }
            }
            "active_speaker" => {
                dom_seen = true;
                let name = e
                    .data
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty());
                // Индикатор ЭКСКЛЮЗИВЕН: новый активный закрывает предыдущего.
                if let Some((prev, start)) = open_dom.take() {
                    if Some(&prev) == name.as_ref() {
                        open_dom = Some((prev, start));
                        continue;
                    }
                    push_dom(&mut speakers_dom, &mut speaker_pos, &prev, start, ts);
                }
                if let Some(name) = name {
                    open_dom = Some((name, ts));
                }
            }
            _ => {}
        }
    }
    // Незакрытое в конце записи: бот вышел, а «конец речи» не пришёл.
    let mut leftovers: Vec<(String, i64)> = open_track.into_iter().collect();
    leftovers.sort();
    for (id, start) in leftovers {
        if let Some(slot) = track_pos.get(&id) {
            if end > start {
                tracks[*slot].speech.push(Span::new(start, end));
            }
        }
    }
    if let Some((name, start)) = open_dom {
        push_dom(&mut speakers_dom, &mut speaker_pos, &name, start, end);
    }

    let report = attrib::attribute(&tracks, &speakers_dom, &[], cfg);

    // Выбор источника — см. таблицу в документации функции.
    //
    // ПОДПИСИ ДОРОЖЕК СОБИРАЮТСЯ ЗДЕСЬ ЖЕ, в той же ветке, что и события:
    // нарезка (`pertrack`) обязана называть дорожку ТОЧНО ТАК ЖЕ, как названы
    // её интервалы. Разойдись эти два имени — и «в это время говорил кто-то
    // ещё» срабатывало бы на самом себе: каждая реплика уехала бы в транскрипт
    // с пометкой перебивания.
    let mut labelled: Vec<TimelineTrack> = Vec::new();
    let (events, source): (Vec<SpeechEvent>, &str) = if report.named() > 0 {
        let mut out = Vec::new();
        for (t, b) in tracks.iter().zip(report.bindings.iter()) {
            let label = b.label();
            for span in &t.speech {
                out.push(SpeechEvent::On {
                    speaker: label.clone(),
                    at_ms: span.start_ms,
                });
                out.push(SpeechEvent::Off {
                    speaker: label.clone(),
                    at_ms: span.end_ms,
                });
            }
            labelled.push(TimelineTrack {
                idx: t.idx,
                label,
                speech: t.speech.clone(),
            });
        }
        (out, "vad")
    } else if dom_seen {
        let mut out = Vec::new();
        for s in &speakers_dom {
            for span in &s.active {
                out.push(SpeechEvent::On {
                    speaker: s.name.clone(),
                    at_ms: span.start_ms,
                });
                out.push(SpeechEvent::Off {
                    speaker: s.name.clone(),
                    at_ms: span.end_ms,
                });
            }
        }
        (out, "dom")
    } else {
        let mut out = Vec::new();
        for t in &tracks {
            let label = format!("Дорожка {}", t.idx);
            for span in &t.speech {
                out.push(SpeechEvent::On {
                    speaker: label.clone(),
                    at_ms: span.start_ms,
                });
                out.push(SpeechEvent::Off {
                    speaker: label.clone(),
                    at_ms: span.end_ms,
                });
            }
            labelled.push(TimelineTrack {
                idx: t.idx,
                label,
                speech: t.speech.clone(),
            });
        }
        (out, "vad")
    };

    Timeline {
        intervals: speakers::intervals_from_events(&events, end),
        binding: crate::ingest::manifest::SpeakerBinding {
            stage: report.stage.as_str().to_string(),
            source: source.to_string(),
            named_tracks: report.named(),
            tracks: tracks.len(),
            confidence: report.best_confidence(),
        },
        report,
        // Пер-трековые интервалы остаются пустыми на DOM-пути: ветка `dom`
        // ничего в `labelled` не кладёт. Отдай мы их там, нарезка Телемоста
        // резала бы дорожки, которые человеку не соответствуют, — то есть
        // подписывала бы чужую речь чужим именем с уверенностью 1.0.
        tracks: labelled,
    }
}

/// Собирает задания пер-трековой расшифровки: файл, подпись и своя речь.
///
/// Интервалы берутся ПЕР-ТРЕКОВЫЕ (`timeline.tracks`), а не из общего списка:
/// в общем они уже перемешаны, и понять, какой кусок из какого файла, нельзя.
/// Дрейф применяется здесь же — резать файл, живущий на оси аудио, по меткам с
/// оси событий значит отрезать речь на величину расхождения.
///
/// Пустой результат — законный исход: он означает «пер-трекового VAD нет»
/// (режим `Slots`, где интервалы приходят из DOM) либо «дорожек нет вовсе».
fn per_track_jobs(
    timeline: &Timeline,
    raw_dir: &Path,
    drift_scale: Option<f64>,
) -> Vec<pertrack::TrackJob> {
    let k = drift_scale.filter(|v| v.is_finite() && *v > 0.0);
    let scale = |v: i64| -> i64 {
        match k {
            Some(k) => (v as f64 * k).round() as i64,
            None => v,
        }
    };
    timeline
        .tracks
        .iter()
        .filter(|t| !t.speech.is_empty())
        .map(|t| pertrack::TrackJob {
            idx: t.idx,
            label: t.label.clone(),
            wav: raw_dir.join(format!("track-{}.wav", t.idx)),
            speech: t
                .speech
                .iter()
                .map(|s| (scale(s.start_ms), scale(s.end_ms)))
                .collect(),
        })
        .collect()
}

/// Решает, идти ли по дорожкам, и НАЗЫВАЕТ ПРИЧИНУ отказа.
///
/// Причина возвращается строкой, а не `bool`, потому что она уезжает в журнал
/// встречи: «почему у этой записи спикеры угаданы, а не известны» — первый
/// вопрос, который зададут через месяц.
///
/// | причина | что означает |
/// |---|---|
/// | `pertrack_disabled` | выключено в `meet.toml` |
/// | `no_manifest` | паспорт записи потерян, режим SFU неизвестен |
/// | `mode_not_per_participant` | `Slots` или `Mixed`: дорожка ≠ человек |
/// | `no_vad_intervals` | пер-трекового VAD в журнале нет |
/// | `track_file_missing` | дорожка есть в журнале, файла на диске нет |
/// | `budget_exceeded` | речи по дорожкам больше, чем позволяет бюджет GPU |
fn per_track_decision(
    cfg: &crate::config::MeetConfig,
    manifest: Option<&Manifest>,
    jobs: &[pertrack::TrackJob],
    mix_ms: i64,
    cloud: bool,
) -> Result<(), &'static str> {
    if !cfg.pertrack_transcription {
        return Err("pertrack_disabled");
    }
    let Some(m) = manifest else {
        return Err("no_manifest");
    };
    // `Slots` (Телемост, измерено) и `Mixed` дорожку человеку не
    // соответствуют. Резать по ним значило бы подписать чужую речь чужим
    // именем — причём с уверенностью 1.0, то есть убедительно.
    if !m.track_mode.is_some_and(|mode| mode.tracks_are_people()) {
        return Err("mode_not_per_participant");
    }
    if jobs.is_empty() {
        return Err("no_vad_intervals");
    }
    if jobs.iter().any(|j| !j.wav.is_file()) {
        return Err("track_file_missing");
    }
    let speech: i64 = jobs
        .iter()
        .map(|j| {
            pertrack::plan_cuts(&j.speech, pertrack::CUT_PAD_MS, mix_ms).speech_ms()
        })
        .sum();
    // ЕДИНСТВЕННОЕ, ЧЕМ РАЗЛИЧАЮТСЯ ДВИЖКИ. Локальный бюджет считает оценку
    // ВРЕМЕНИ и включает постоянную цену запуска процесса: whisper-cli заново
    // грузит модель на каждой дорожке. В облаке процесса нет, цена линейна по
    // минутам звука — и локальный критерий отказал бы там, где отказывать не
    // за что (см. `cloud::stt::within_cloud_budget`).
    let fits = if cloud {
        crate::cloud::stt::within_cloud_budget(speech, mix_ms)
    } else {
        pertrack::within_budget(speech, jobs.len(), mix_ms)
    };
    if !fits {
        return Err("budget_exceeded");
    }
    Ok(())
}

/// Добавляет DOM-интервал участнику.
fn push_dom(
    speakers: &mut Vec<crate::bot::attrib::SpeakerTimeline>,
    pos: &mut HashMap<String, usize>,
    name: &str,
    start: i64,
    end: i64,
) {
    if end <= start {
        return;
    }
    let slot = *pos.entry(name.to_string()).or_insert_with(|| {
        speakers.push(crate::bot::attrib::SpeakerTimeline {
            key: format!("name:{}", name.to_lowercase()),
            name: name.to_string(),
            active: Vec::new(),
        });
        speakers.len() - 1
    });
    speakers[slot]
        .active
        .push(crate::bot::attrib::Span::new(start, end));
}

fn field_ms(data: &serde_json::Value) -> Option<i64> {
    data.get("at_ms")
        .or_else(|| data.get("ms"))
        .and_then(serde_json::Value::as_i64)
}

/// Кладёт результат в базу: интервалы и сегменты, оба — заменой.
///
/// Заменой, а не дозаписью: повтор этапа обязан дать тот же результат, что и
/// первый прогон, а не удвоенный.
async fn store_results(
    state: &Arc<MeetState>,
    meeting_id: &str,
    intervals: &[Interval],
    segments: &[AttributedSegment],
    track_of: &HashMap<String, i64>,
) -> Result<(), MeetError> {
    repo::spans::clear(state, meeting_id).await?;
    let spans: Vec<NewSpan> = intervals
        .iter()
        .map(|i| NewSpan {
            participant_id: None,
            track_idx: track_of.get(&i.speaker).copied(),
            start_ms: i.start_ms,
            end_ms: i.end_ms,
            // Источник один на список: журнал не различает их после слияния,
            // а различать в базе то, чего нет в данных, — врать.
            source: "timeline".to_string(),
        })
        .collect();
    repo::spans::insert_many(state, meeting_id, spans).await?;

    let rows: Vec<NewSegment> = segments
        .iter()
        .enumerate()
        .map(|(idx, s)| NewSegment {
            idx: idx as i64,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            text: s.text.clone(),
            participant_id: None,
            speaker_name: s.speaker.clone(),
            track_idx: s.speaker.as_ref().and_then(|n| track_of.get(n)).copied(),
            confidence: Some(s.confidence),
            overlapped: s.overlapped,
            words_json: (!s.words.is_empty())
                .then(|| serde_json::to_string(&words_dto(s)).unwrap_or_default()),
        })
        .collect();
    let n = repo::segments::replace_all(state, meeting_id, rows).await?;

    // Фронт узнаёт о готовой расшифровке кадром, а не опросом.
    if let Ok(page) = repo::segments::page(state, meeting_id, 0, 200).await {
        state.broadcast_event(MeetServerMsg::TranscriptDelta {
            meeting_id: meeting_id.to_string(),
            segments: page,
        });
    }
    tracing::debug!(target: "forge_meet", meeting_id, segments = n, "forge-meet: сегменты сохранены");
    Ok(())
}

fn words_dto(s: &AttributedSegment) -> Vec<render::WordDto> {
    s.words
        .iter()
        .map(|w| render::WordDto {
            t: w.text.clone(),
            s: w.start_ms,
            e: w.end_ms,
        })
        .collect()
}
