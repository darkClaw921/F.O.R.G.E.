//! Сеанс записи: связка бота, слота, рекордера и статус-машины.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Что здесь происходит
//!
//! `POST /api/meet/meetings` создаёт строку в базе и просит [`start`] завести
//! сеанс. Дальше всё идёт само:
//!
//! ```text
//! BotFactory ──► dyn MeetBot ──► BotStreams (события + звук)
//!                                     │
//!                        ┌────────────┴────────────┐
//!                        ▼                         ▼
//!                 статус-машина              Recorder (WAV, журнал,
//!               (repo::meetings)              manifest.json)
//! ```
//!
//! # Почему бот приходит ФАБРИКОЙ, а не создаётся здесь
//!
//! Ф3 обязана проверять весь пайплайн БЕЗ БРАУЗЕРА, а Ф4 — подставить живой
//! Chrome, не тронув ни строчки этого модуля. Фабрика — единственная точка,
//! где это решается: тест ставит [`ScriptedBotFactory`], `main` поставит
//! `ChromeBotFactory`.
//!
//! Пока фабрика не установлена, старт записи отвечает `409
//! chrome_unavailable` — честно и в терминах лестницы ошибок, а не паникой.
//!
//! # Слот держится СЕАНСОМ
//!
//! [`Slot`](crate::hub::Slot) лежит внутри [`RecordingSession`] и
//! освобождается вместе с ней — в том числе при панике воркера. Держи мы слот
//! в переменной воркера, паника оставила бы его занятым навсегда, и после двух
//! таких падений плагин перестал бы принимать записи до перезапуска devforge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::bot::{BotStreams, JoinMode, MeetBot, ScriptedBot, WindowReport};
use crate::db::repo::{meetings, MeetingRow};
use crate::error::MeetError;
use crate::hub::event::{MeetEvent, StopReason};
use crate::hub::Slot;
use crate::ingest::{Manifest, Recorder};
use crate::state::MeetState;
use crate::status::MeetStatus;
use crate::ws::protocol::MeetServerMsg;

/// Заявка на бота.
#[derive(Debug, Clone)]
pub struct BotRequest {
    pub meeting_id: String,
    /// Канонический URL комнаты.
    pub url: String,
    pub mode: JoinMode,
    /// Каталог встречи: профиль Chrome, `chrome.pid`, WAV, журнал.
    pub meeting_dir: PathBuf,
    /// Имя, под которым бот виден участникам.
    pub bot_name: String,
}

/// Источник ботов.
pub trait BotFactory: Send + Sync {
    /// Создаёт бота для встречи.
    ///
    /// Синхронная: запуск браузера идёт внутри [`MeetBot::start`], а здесь
    /// только конструирование. Так `POST /meetings` отвечает быстро и не ждёт
    /// Chrome.
    fn create(&self, req: &BotRequest) -> Result<Arc<dyn MeetBot>, MeetError>;
}

/// Фабрика, которой нет.
///
/// Стоит по умолчанию до Ф4. Отвечает кодом лестницы ошибок, а не паникой:
/// плагин обязан стартовать всегда и честно рассказывать, чего не хватает.
pub struct MissingBotFactory;

impl BotFactory for MissingBotFactory {
    fn create(&self, _req: &BotRequest) -> Result<Arc<dyn MeetBot>, MeetError> {
        Err(MeetError::DepUnavailable("chrome_unavailable"))
    }
}

/// Фабрика сценарных ботов — для тестов и e2e без браузера.
pub struct ScriptedBotFactory {
    /// Сценарий, который получит каждый бот.
    pub script: crate::bot::BotScript,
    /// Созданные боты в порядке появления: тест может спросить, что бот
    /// делал со страницей.
    created: std::sync::Mutex<Vec<Arc<ScriptedBot>>>,
}

impl ScriptedBotFactory {
    pub fn new(script: crate::bot::BotScript) -> Self {
        Self {
            script,
            created: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Последний созданный бот.
    pub fn last(&self) -> Option<Arc<ScriptedBot>> {
        self.created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }
}

impl BotFactory for ScriptedBotFactory {
    fn create(&self, _req: &BotRequest) -> Result<Arc<dyn MeetBot>, MeetError> {
        let bot = Arc::new(ScriptedBot::new(self.script.clone()));
        self.created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(bot.clone());
        Ok(bot)
    }
}

/// Идущий сеанс записи.
pub struct RecordingSession {
    pub meeting_id: String,
    pub mode: JoinMode,
    bot: Arc<dyn MeetBot>,
    /// Почему нужен человек: `captcha` | `login` | `lobby`. `None` — не нужен.
    needs_human: RwLock<Option<String>>,
    /// Гасит воркер записи.
    pub cancel: CancellationToken,
    /// Причина остановки, названная ЧЕЛОВЕКОМ.
    ///
    /// Поле существует ради единственной, но принципиальной разницы: `stop`
    /// значит «встреча кончилась, обработай» (файлы остаются), а `cancel` —
    /// «прерви и выброси». Оба приходят через один и тот же токен отмены, и
    /// без явно записанной причины воркер не смог бы их различить — то есть
    /// отменённая встреча уезжала бы в пост-обработку вместе со звуком,
    /// который просили выбросить.
    requested: std::sync::Mutex<Option<StopReason>>,
    /// Слот записи. Живёт ровно столько же, сколько сеанс.
    _slot: Slot,
}

impl std::fmt::Debug for RecordingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Руками, а не derive: за `bot` стоит `dyn MeetBot`, у которого
        // `Debug` нет и быть не обязано — трейт описывает поведение, а не
        // представление.
        f.debug_struct("RecordingSession")
            .field("meeting_id", &self.meeting_id)
            .field("mode", &self.mode)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

impl RecordingSession {
    /// Нужен ли человек прямо сейчас.
    pub async fn needs_human(&self) -> Option<String> {
        self.needs_human.read().await.clone()
    }

    /// «Я внутри — пиши».
    pub async fn confirm_human_ready(&self) -> Result<(), MeetError> {
        self.bot.confirm_human_ready().await?;
        *self.needs_human.write().await = None;
        Ok(())
    }

    /// Показывает окно бота человеку либо убирает обратно.
    pub async fn show_window(&self, visible: bool) -> Result<WindowReport, MeetError> {
        self.bot.show_window(visible).await
    }

    /// «Что видит бот» — PNG.
    pub async fn screenshot(&self) -> Result<Vec<u8>, MeetError> {
        self.bot.screenshot().await
    }

    /// Просит остановить запись С НАЗВАННОЙ ПРИЧИНОЙ.
    ///
    /// Причина решает судьбу файлов: [`StopReason::UserCancel`] выбрасывает
    /// каталог, всё остальное — сохраняет и ведёт в пост-обработку.
    pub async fn request_stop(&self, reason: StopReason) -> Result<(), MeetError> {
        *self.requested.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        let res = self.bot.stop(reason).await;
        // Токен отменяется ПОСЛЕ записи причины: воркер просыпается сразу и
        // обязан прочитать уже проставленное значение.
        self.cancel.cancel();
        res
    }

    /// Причина, названная человеком, либо `None`.
    pub fn requested_stop(&self) -> Option<StopReason> {
        *self.requested.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Реестр идущих сеансов.
#[derive(Default)]
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<RecordingSession>>>,
}

impl SessionRegistry {
    pub async fn get(&self, meeting_id: &str) -> Option<Arc<RecordingSession>> {
        self.sessions.read().await.get(meeting_id).cloned()
    }

    pub async fn insert(&self, session: Arc<RecordingSession>) {
        self.sessions
            .write()
            .await
            .insert(session.meeting_id.clone(), session);
    }

    pub async fn remove(&self, meeting_id: &str) -> Option<Arc<RecordingSession>> {
        self.sessions.write().await.remove(meeting_id)
    }

    pub async fn len(&self) -> usize {
        self.sessions.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Запускает запись встречи.
///
/// Слот записи ДОЛЖЕН быть уже занят вызывающим: проверка слота стоит в
/// лестнице ошибок раньше резолва встречи, и занимать его здесь означало бы
/// нарушить порядок.
pub async fn start(
    state: &Arc<MeetState>,
    meeting: &MeetingRow,
    mode: JoinMode,
    slot: Slot,
) -> Result<Arc<RecordingSession>, MeetError> {
    let factory = state.bot_factory();
    let req = BotRequest {
        meeting_id: meeting.id.clone(),
        url: meeting.canonical_url.clone(),
        mode,
        meeting_dir: PathBuf::from(&meeting.meeting_dir),
        bot_name: meeting
            .bot_name
            .clone()
            .unwrap_or_else(|| state.config.bot_name.clone()),
    };
    let bot = factory.create(&req)?;

    let session = Arc::new(RecordingSession {
        meeting_id: meeting.id.clone(),
        mode,
        bot: bot.clone(),
        needs_human: RwLock::new(None),
        cancel: state.shutdown.child_token(),
        requested: std::sync::Mutex::new(None),
        _slot: slot,
    });
    state.sessions.insert(session.clone()).await;
    state.hub.mark_active(&meeting.id).await;

    meetings::set_status(state, &meeting.id, MeetStatus::Launching, Some("bot_start")).await?;

    let streams = match bot.start(mode).await {
        Ok(s) => s,
        Err(e) => {
            // Слот освободится вместе с сеансом — поэтому убираем сеанс сразу,
            // а не «когда-нибудь».
            state.sessions.remove(&meeting.id).await;
            state.hub.mark_inactive(&meeting.id).await;
            let _ = meetings::set_status(state, &meeting.id, MeetStatus::Failed, Some(e.code())).await;
            return Err(e);
        }
    };

    let manifest = Manifest::new(
        &meeting.id,
        &meeting.canonical_url,
        meeting.platform.as_str(),
        mode.as_str(),
        state.config.audio_sample_rate,
    );
    let recorder = Recorder::create(&meeting.meeting_dir, manifest)
        .map_err(|e| MeetError::Internal(format!("каталог встречи не готов: {e}")))?;

    let worker_state = state.clone();
    let worker_session = session.clone();
    let handle = tokio::spawn(async move {
        run_worker(worker_state, worker_session, streams, recorder).await;
    });
    state.register_worker(handle).await;

    Ok(session)
}

/// Воркер записи: тянет события и звук, ведёт статусы и файлы.
async fn run_worker(
    state: Arc<MeetState>,
    session: Arc<RecordingSession>,
    mut streams: BotStreams,
    mut recorder: Recorder,
) {
    let meeting_id = session.meeting_id.clone();
    let mut stop_reason: Option<StopReason> = None;
    let mut failure: Option<(String, String)> = None;

    loop {
        tokio::select! {
            // biased: события ведут статусы, и обработать «дорожка появилась»
            // раньше её первого кадра дешевле, чем разбираться потом, почему
            // дорожка завелась «неявно».
            biased;

            _ = session.cancel.cancelled() => {
                // Причину называет тот, кто отменил: `cancel` выбрасывает
                // файлы, `stop` и graceful shutdown — сохраняют.
                stop_reason = Some(session.requested_stop().unwrap_or(StopReason::Shutdown));
                break;
            }

            event = streams.events.recv() => {
                match event {
                    Some(ev) => {
                        recorder.note_meet_event(&ev);
                        if let Some(reason) = apply_event(&state, &session, &ev).await {
                            stop_reason = Some(reason);
                            break;
                        }
                        if let MeetEvent::Failed { code, message } = &ev {
                            failure = Some((code.clone(), message.clone()));
                            break;
                        }
                    }
                    None => {
                        // Канал событий закрылся — бот ушёл. Звук мог не
                        // успеть долиться, поэтому дочитываем его ниже.
                        stop_reason = Some(StopReason::BrowserGone);
                        break;
                    }
                }
            }

            frame = streams.audio.recv() => {
                match frame {
                    Some(f) => {
                        if let Err(e) = recorder.accept(crate::bot::transport::PageMessage::Audio(f)) {
                            tracing::warn!(
                                target: "forge_meet",
                                meeting_id = %meeting_id,
                                error = %e,
                                "forge-meet: кадр не записался"
                            );
                        }
                    }
                    None => {
                        // Звук кончился, события ещё могут идти — продолжаем.
                    }
                }
            }
        }
    }

    // Дочитываем хвост звука: worklet дофлашивает буфер при закрытии, и терять
    // последние полсекунды встречи незачем.
    while let Ok(f) = streams.audio.try_recv() {
        let _ = recorder.accept(crate::bot::transport::PageMessage::Audio(f));
    }

    let reason = stop_reason.unwrap_or(StopReason::BrowserGone);
    let _ = session.bot.stop(reason).await;

    let report = match recorder.finish() {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                error = %e,
                "forge-meet: запись не закрылась штатно"
            );
            None
        }
    };

    state.sessions.remove(&meeting_id).await;
    state.hub.mark_inactive(&meeting_id).await;

    if let Some((code, message)) = failure {
        let _ = meetings::set_status(&state, &meeting_id, MeetStatus::Failed, Some(&code)).await;
        state.broadcast_event(MeetServerMsg::Error {
            meeting_id: Some(meeting_id.clone()),
            code,
            message,
        });
        return;
    }

    // ОСТАНОВКА ДО НАЧАЛА ЗАПИСИ — ОТДЕЛЬНЫЙ ИСХОД, А НЕ ЧАСТНЫЙ СЛУЧАЙ
    // ШТАТНОГО.
    //
    // Ветка ниже вела встречу через `Leaving` → `Transcoding`, и оба перехода
    // разрешены ТОЛЬКО из `Recording`. Если бот остановился, не дойдя до
    // записи — а это рядовой случай: вход в несуществующую комнату даёт
    // маркер «Встреча завершена» за две секунды, Chrome умирает на входе, —
    // оба `set_status` возвращали `InvalidState`, ошибка гасилась через
    // `let _`, пост-обработка не ставилась, и строка НАВСЕГДА оставалась в
    // `launching`.
    //
    // Стоило это не только зависшей карточки. `launching` входит в
    // `MeetStatus::ACTIVE` и в частичный уникальный индекс
    // `ux_meetings_active_url`, поэтому любой следующий старт по ТОМУ ЖЕ
    // адресу прицеплялся к мёртвой строке и возвращал `attached: true` без
    // единого работающего бота. Комната становилась незапускаемой до ручной
    // отмены или перезапуска devforge.
    //
    // Мерило — статус в базе, а не число кадров: «дошли до записи» это ровно
    // то, что решает, законны ли переходы ниже.
    let recorded = matches!(
        meetings::get(&state, &meeting_id).await.map(|r| r.status),
        Ok(MeetStatus::Recording) | Ok(MeetStatus::Leaving)
    );

    if !recorded {
        // Записывать нечего: пост-обработке не с чем работать, и вести
        // встречу в `Transcoding` значило бы отдать ffmpeg пустой каталог.
        // `Failed` и `Cancelled` из `launching` таблица переходов разрешает,
        // расширять её не требуется.
        let target = if reason == StopReason::DomEnded {
            MeetStatus::Failed
        } else {
            MeetStatus::Cancelled
        };
        if let Err(e) =
            meetings::set_status(&state, &meeting_id, target, Some(reason.as_str())).await
        {
            tracing::error!(
                target: "forge_meet",
                meeting_id = %meeting_id,
                error = %e,
                reason = reason.as_str(),
                "forge-meet: встреча не переведена в терминальный статус — строка останется активной"
            );
        }
        if let Ok(row) = meetings::get(&state, &meeting_id).await {
            let _ = std::fs::remove_dir_all(&row.meeting_dir);
        }
        state.broadcast_event(MeetServerMsg::Error {
            meeting_id: Some(meeting_id.clone()),
            code: reason.as_str().to_string(),
            message: if reason == StopReason::DomEnded {
                "Встреча уже завершена — записывать нечего".to_string()
            } else {
                "Бот не дошёл до записи".to_string()
            },
        });
        tracing::info!(
            target: "forge_meet",
            meeting_id = %meeting_id,
            reason = reason.as_str(),
            status = target.as_str(),
            "forge-meet: остановка до начала записи"
        );
    } else if reason.keeps_files() {
        // Штатный путь: выходим из встречи и уходим в пост-обработку.
        let _ = meetings::set_status(&state, &meeting_id, MeetStatus::Leaving, Some(reason.as_str())).await;
        let moved = meetings::set_status(&state, &meeting_id, MeetStatus::Transcoding, Some(reason.as_str())).await;
        if moved.is_ok() {
            // Пост-обработка ставится ПОСЛЕ снятия встречи с учёта активных
            // (`mark_inactive` выше): иначе она сама себя увидела бы идущей
            // записью и ждала бы её конца вечно.
            crate::pipeline::run::spawn(&state, &meeting_id);
        }
    } else {
        // «Прерви и выброси»: файлы удаляются.
        if let Ok(row) = meetings::get(&state, &meeting_id).await {
            if let Err(e) = std::fs::remove_dir_all(&row.meeting_dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        target: "forge_meet",
                        meeting_id = %meeting_id,
                        error = %e,
                        "forge-meet: каталог отменённой встречи не удалился"
                    );
                }
            }
        }
        let _ = meetings::set_status(&state, &meeting_id, MeetStatus::Cancelled, Some(reason.as_str())).await;
    }

    if let Some(report) = report {
        tracing::info!(
            target: "forge_meet",
            meeting_id = %meeting_id,
            tracks = report.tracks,
            frames = report.frames,
            duration_ms = report.duration_ms,
            dropped = report.dropped_by_page,
            reason = reason.as_str(),
            "forge-meet: запись закрыта"
        );
    }
}

/// Отражает событие бота в статусе встречи. Возвращает причину остановки,
/// если событие её объявляет.
async fn apply_event(
    state: &Arc<MeetState>,
    session: &Arc<RecordingSession>,
    ev: &MeetEvent,
) -> Option<StopReason> {
    let id = &session.meeting_id;
    match ev {
        MeetEvent::Joined => {
            let _ = meetings::set_status(state, id, MeetStatus::Joining, Some("joined")).await;
        }
        MeetEvent::WaitingAdmit => {
            // Лобби площадки: вход нажат, ждём решения организатора. Человек
            // здесь не нужен — нужен статус, по которому видно, что бот не
            // завис, а стоит в очереди.
            let _ = meetings::set_status(state, id, MeetStatus::WaitingAdmit, Some("lobby")).await;
        }
        MeetEvent::NeedsHuman { kind } => {
            *session.needs_human.write().await = Some(kind.clone());
            // Статус не меняется: `waiting_admit` — это лобби платформы, а
            // здесь может быть капча ещё до входа. Фронт узнаёт о нужде в
            // человеке кадром, а не статусом.
            state.broadcast_event(MeetServerMsg::NeedsHuman {
                meeting_id: id.clone(),
                kind: kind.clone(),
            });
        }
        MeetEvent::RecordingStarted { .. } => {
            let _ = meetings::set_status(state, id, MeetStatus::Recording, Some("audio_started")).await;
        }
        MeetEvent::RecordingStopped { reason, .. } => return Some(*reason),
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::BotScript;
    use crate::providers::parse_meeting_url;

    async fn stand(script: BotScript) -> (Arc<MeetState>, tempfile::TempDir, Arc<ScriptedBotFactory>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            ..crate::config::MeetConfig::default()
        };
        let state = MeetState::new(cfg);
        let db = crate::db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let factory = Arc::new(ScriptedBotFactory::new(script));
        state.set_bot_factory(factory.clone());
        (state, dir, factory)
    }

    async fn create_meeting(state: &Arc<MeetState>, dir: &std::path::Path) -> MeetingRow {
        let link = parse_meeting_url("https://telemost.yandex.ru/j/60497752704486").unwrap();
        let out = meetings::create_or_attach(
            state,
            meetings::NewMeeting {
                link,
                source: None,
                title: None,
                mode: "normal".into(),
                bot_name: "F.O.R.G.E.".into(),
                meeting_dir: dir.join("meetings/m1").to_string_lossy().to_string(),
            },
        )
        .await
        .unwrap();
        out.meeting
    }

    async fn wait_for_status(state: &Arc<MeetState>, id: &str, want: MeetStatus) -> MeetStatus {
        for _ in 0..200 {
            let row = meetings::get(state, id).await.unwrap();
            if row.status == want || row.status.is_terminal() {
                return row.status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        meetings::get(state, id).await.unwrap().status
    }

    /// Полный путь сеанса: слот занят, статусы прошли, файлы на диске.
    #[tokio::test]
    async fn a_session_records_and_lands_in_transcoding() {
        let (state, dir, _f) = stand(BotScript::two_speakers_overlapping(16_000, 20)).await;
        let meeting = create_meeting(&state, dir.path()).await;

        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap();
        assert_eq!(session.mode, JoinMode::Normal);

        let status = wait_for_status(&state, &meeting.id, MeetStatus::Transcoding).await;
        assert_eq!(status, MeetStatus::Transcoding, "запись обязана уйти в пост-обработку");

        // Сеанс убран из реестра; слот вернётся, когда уйдёт последняя ссылка
        // на сеанс — он и есть владелец слота.
        assert!(state.sessions.get(&meeting.id).await.is_none());
        drop(session);
        assert_eq!(state.hub.recordings_available(), state.limits().recordings);

        let m = Manifest::load(std::path::Path::new(&meeting.meeting_dir)).unwrap();
        assert!(m.finished);
        assert_eq!(m.tracks.len(), 2, "две дорожки сценария");
        assert_eq!(m.capture_path.as_deref(), Some("rtcpeerconnection"));
        assert!(m.duration_ms >= 7_000, "длительность {}", m.duration_ms);

        for idx in 0..2 {
            let p = std::path::Path::new(&meeting.meeting_dir)
                .join("raw")
                .join(format!("track-{idx}.wav"));
            assert!(p.is_file(), "нет {}", p.display());
            let samples = crate::ingest::wav::read_samples(&p).unwrap();
            assert!(samples.len() > 16_000, "дорожка {idx} почти пуста: {}", samples.len());
            assert!(samples.iter().any(|&s| s != 0), "дорожка {idx} — тишина");
        }
    }

    /// Отмена ВЫБРАСЫВАЕТ данные, а остановка — сохраняет. Это единственная
    /// пара команд, ради разницы между которыми они и разделены.
    #[tokio::test]
    async fn cancel_throws_the_recording_away_and_stop_keeps_it() {
        // stop
        let (state, dir, _f) = stand(BotScript::two_speakers_overlapping(16_000, 20)).await;
        let meeting = create_meeting(&state, dir.path()).await;
        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap();
        wait_for_status(&state, &meeting.id, MeetStatus::Transcoding).await;
        drop(session);
        assert!(
            std::path::Path::new(&meeting.meeting_dir).join("raw").is_dir(),
            "stop обязан сохранить файлы"
        );

        // cancel — сценарий проигрывается с паузой, иначе запись успела бы
        // кончиться сама раньше, чем тест дойдёт до команды.
        let slow = BotScript {
            playback_delay_ms: 20,
            ..BotScript::two_speakers_overlapping(16_000, 20)
        };
        let (state, dir, _f) = stand(slow).await;
        let meeting = create_meeting(&state, dir.path()).await;
        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap();
        wait_for_status(&state, &meeting.id, MeetStatus::Recording).await;
        let meeting_dir = meeting.meeting_dir.clone();
        assert!(std::path::Path::new(&meeting_dir).join("raw").is_dir());

        session.request_stop(StopReason::UserCancel).await.unwrap();
        let status = wait_for_status(&state, &meeting.id, MeetStatus::Cancelled).await;
        assert_eq!(status, MeetStatus::Cancelled, "отменённая встреча обязана быть cancelled");
        assert!(
            !std::path::Path::new(&meeting_dir).exists(),
            "cancel обязан ВЫБРОСИТЬ данные: каталог {meeting_dir} на месте"
        );
    }

    /// ОСТАНОВКА ДО НАЧАЛА ЗАПИСИ ОБЯЗАНА КОНЧИТЬСЯ ТЕРМИНАЛЬНЫМ СТАТУСОМ.
    ///
    /// Сценарий без единого события — это бот, у которого поток закрылся, не
    /// дойдя до записи. Живьём так выглядят вход в несуществующую комнату
    /// (маркер «Встреча завершена» за две секунды) и смерть Chrome на входе.
    ///
    /// Раньше воркер вёл такую встречу через `Leaving` → `Transcoding`, оба
    /// перехода из `launching` запрещены таблицей, ошибка гасилась через
    /// `let _`, и строка оставалась в `launching` НАВСЕГДА. Цена — не только
    /// зависшая карточка: `launching` входит в `ux_meetings_active_url`,
    /// поэтому следующий старт по тому же адресу цеплялся к мёртвой строке и
    /// возвращал `attached: true` без единого работающего бота.
    #[tokio::test]
    async fn a_stop_before_recording_ends_in_a_terminal_status() {
        let (state, dir, _f) = stand(BotScript::default()).await;
        let meeting = create_meeting(&state, dir.path()).await;

        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap();

        let mut status = MeetStatus::Launching;
        for _ in 0..300 {
            status = meetings::get(&state, &meeting.id).await.unwrap().status;
            if status.is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            status.is_terminal(),
            "встреча зависла в {status:?}: строка осталась активной и заняла адрес навсегда"
        );

        // Слот обязан вернуться: иначе один промах входа съедал бы место в
        // очереди записей до перезапуска.
        //
        // Ждём, а не проверяем сразу. `start` отдаёт `Arc<RecordingSession>`, и
        // слот лежит ВНУТРИ сеанса — то есть возвращается он не в момент
        // `drop` здесь, а когда свою копию `Arc` отпустит воркер. Это разные
        // моменты, и синхронная проверка ловила не утечку слота, а то, успел
        // ли воркер доработать: изолированно тест проходил всегда, а в полном
        // параллельном прогоне падал с `left: 1, right: 2`.
        //
        // Ожидание настоящую утечку не прячет: слот, который не вернулся, не
        // вернётся и за секунду, и тест упадёт — просто с честной причиной.
        drop(session);
        let want = state.limits().recordings;
        for _ in 0..100 {
            if state.hub.recordings_available() == want {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            state.hub.recordings_available(),
            want,
            "слот записи не вернулся за секунду после завершения сеанса: \
             один промах входа съедает место в очереди до перезапуска"
        );
    }

    /// Без фабрики (до Ф4) старт отвечает кодом лестницы ошибок, а не паникой.
    #[tokio::test]
    async fn without_a_factory_the_start_answers_chrome_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            ..crate::config::MeetConfig::default()
        };
        let state = MeetState::new(cfg);
        let db = crate::db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        state.attach_db(Arc::new(db)).unwrap();

        let meeting = create_meeting(&state, dir.path()).await;
        let slot = state.hub.try_acquire_recording().unwrap();
        let err = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap_err();
        assert_eq!(err, MeetError::DepUnavailable("chrome_unavailable"));
    }

    /// АССИСТИРОВАННЫЙ ВХОД: бот ничего не кликает, пока человек не скажет.
    #[tokio::test]
    async fn assisted_mode_waits_for_the_human_before_recording() {
        let (state, dir, factory) = stand(BotScript::two_speakers_overlapping(16_000, 20)).await;
        let meeting = create_meeting(&state, dir.path()).await;

        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Assisted, slot).await.unwrap();

        // Даём воркеру прокрутиться: запись НЕ должна начаться сама.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let row = meetings::get(&state, &meeting.id).await.unwrap();
        assert_eq!(row.status, MeetStatus::Launching, "запись пошла без человека");

        let bot = factory.last().unwrap();
        assert_eq!(bot.click_count(), 0, "бот кликал сам: {:?}", bot.actions());

        session.confirm_human_ready().await.unwrap();
        let status = wait_for_status(&state, &meeting.id, MeetStatus::Transcoding).await;
        assert_eq!(status, MeetStatus::Transcoding);
        assert_eq!(bot.click_count(), 0, "подтверждение не кликает за человека");
    }

    /// `needs_human` доезжает до сеанса и снимается подтверждением.
    #[tokio::test]
    async fn needs_human_is_visible_and_clears_on_confirmation() {
        let script = crate::bot::scripted::BotScript {
            join: crate::bot::scripted::JoinOutcome::NeedsHuman("captcha".into()),
            ..BotScript::two_speakers_overlapping(16_000, 20)
        };
        let (state, dir, _f) = stand(script).await;
        let meeting = create_meeting(&state, dir.path()).await;

        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Assisted, slot).await.unwrap();

        for _ in 0..100 {
            if session.needs_human().await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(session.needs_human().await.as_deref(), Some("captcha"));

        // И скриншот доступен именно тогда, когда он нужен.
        let png = session.screenshot().await.unwrap();
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);

        session.confirm_human_ready().await.unwrap();
        assert_eq!(session.needs_human().await, None);
    }

    /// Показ окна возвращает фактические границы, а не запрошенные.
    #[tokio::test]
    async fn showing_the_window_reports_the_real_bounds() {
        let (state, dir, _f) = stand(BotScript::two_speakers_overlapping(16_000, 20)).await;
        let meeting = create_meeting(&state, dir.path()).await;
        let slot = state.hub.try_acquire_recording().unwrap();
        let session = start(&state, &meeting, JoinMode::Normal, slot).await.unwrap();

        let shown = session.show_window(true).await.unwrap();
        assert!(shown.visible && shown.left >= 0);
        let hidden = session.show_window(false).await.unwrap();
        assert!(!hidden.visible);
        assert_eq!(hidden.left, -930, "оконный сервер macOS подрезает координаты");
    }
}
