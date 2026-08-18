//! [`ScriptedBot`] — детерминированный бот по сценарию, БЕЗ БРАУЗЕРА.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Зачем он публичный
//!
//! Он не только для тестов этого крейта. Любой, кто захочет проверить пайплайн
//! записи, — интеграционный тест, e2e, будущий адаптер платформы — обязан
//! иметь возможность подставить встречу без Chrome. Приватный тестовый двойник
//! означал бы, что интеграционные тесты (`tests/*.rs` — отдельный крейт!)
//! вынуждены поднимать браузер.
//!
//! # Детерминизм здесь — требование, а не свойство
//!
//! Сценарий — это ДАННЫЕ: список шагов с уже проставленными метками времени.
//! Никаких `sleep`, никакого системного времени, никакой зависимости от
//! планировщика. Два прогона одного сценария дают байт в байт один поток
//! событий и один и тот же PCM — на это есть тест.
//!
//! Из этого же следует, что тест «двое перебивают друг друга» идёт
//! миллисекунды: проигрывание не ждёт реального времени, оно просто отдаёт
//! заранее посчитанные кадры.
//!
//! # Что бот УМЕЕТ изображать
//!
//! Появление и исчезновение треков (включая renegotiation — трек, начавшийся
//! посреди встречи), интервалы речи с намеренным наложением, список
//! участников, смену активного спикера, конец встречи и **отказ входа с
//! `needs_human`** — ради последнего он и понадобился ассистированному входу.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::frame::AudioFrame;
use super::mode::TrackMode;
use super::{BotStreams, JoinMode, MeetBot, Observation, TrackObservation, WindowReport, AUDIO_CHANNEL_CAPACITY};
use crate::error::MeetError;
use crate::hub::event::{MeetEvent, ParticipantInfo, StopReason};

/// Шаг сценария. Порядок шагов — и есть порядок на проводе.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptStep {
    /// Событие жизненного цикла.
    Event(MeetEvent),
    /// Кадр звука.
    Audio(AudioFrame),
}

/// Чем кончается попытка входа.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JoinOutcome {
    /// Бот вошёл.
    #[default]
    Ok,
    /// Нужен человек: капча, логин, лобби. Сценарий доигрывается ТОЛЬКО после
    /// [`MeetBot::confirm_human_ready`].
    NeedsHuman(String),
    /// Вход не удался совсем.
    Fail { code: String, message: String },
}

/// Сценарий встречи.
#[derive(Debug, Clone)]
pub struct BotScript {
    /// Шаги в порядке проигрывания.
    pub steps: Vec<ScriptStep>,
    /// Чем кончается вход.
    pub join: JoinOutcome,
    /// Что отдаёт `screenshot()`. По умолчанию — минимальный валидный PNG.
    pub screenshot: Vec<u8>,
    /// Что видно в `observe()`.
    pub observation: Observation,
    /// Границы окна, которые «получились» после показа. Разные для видимого и
    /// спрятанного состояния — так же, как у настоящего окна.
    pub visible_bounds: (i32, i32, u32, u32),
    pub hidden_bounds: (i32, i32, u32, u32),
    /// Пауза после каждого СОБЫТИЯ при проигрывании, мс. По умолчанию ноль —
    /// сценарий проигрывается мгновенно, и восьмисекундная встреча стоит
    /// миллисекунд.
    ///
    /// Ненулевое значение нужно ровно одному классу тестов: тем, что
    /// вмешиваются В ХОД записи (отмена на середине, показ окна во время
    /// встречи). Без паузы сценарий успевает доиграть до конца раньше, чем
    /// тест дойдёт до своей команды, и проверка молча превращается в
    /// проверку уже завершённой записи.
    ///
    /// На СОДЕРЖИМОЕ потока пауза не влияет: метки времени лежат в сценарии,
    /// а не берутся из часов.
    pub playback_delay_ms: u64,
}

/// Восьмибайтовая сигнатура PNG плюс пустой IHDR: `screenshot()` обязан
/// отдавать что-то, что вызывающий признает картинкой, даже когда браузера нет.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // сигнатура PNG
    0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', // длина + тип чанка
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
];

impl Default for BotScript {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            join: JoinOutcome::Ok,
            screenshot: TINY_PNG.to_vec(),
            observation: Observation::default(),
            visible_bounds: (80, 80, 1280, 800),
            // Ровно то, что измерено в Ф1: запрошенный `left: -4000` система
            // подрезает до -930, и на экране остаётся полоса.
            hidden_bounds: (-930, 30, 1280, 800),
            playback_delay_ms: 0,
        }
    }
}

/// Интервал речи в сценарии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechInterval {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl SpeechInterval {
    pub fn new(start_ms: u64, end_ms: u64) -> Self {
        Self { start_ms, end_ms }
    }

    pub fn contains(self, t_ms: u64) -> bool {
        t_ms >= self.start_ms && t_ms < self.end_ms
    }
}

/// Описание одной дорожки в генераторе сценариев.
#[derive(Debug, Clone)]
pub struct ScriptedTrack {
    pub track_id: String,
    pub idx: usize,
    /// Когда трек появился на общей оси. Ненулевое значение изображает
    /// renegotiation.
    pub start_offset_ms: u64,
    /// Когда трек исчез.
    pub end_offset_ms: u64,
    /// Интервалы речи внутри трека.
    pub speech: Vec<SpeechInterval>,
    /// Частота тона, Гц. Разная у разных дорожек — тогда по звуку видно, чей
    /// файл открыт.
    pub tone_hz: f32,
}

impl BotScript {
    /// **Опорный сценарий фазы: двое говорят, перебивая друг друга.**
    ///
    /// Перебивание здесь не украшение, а причина существования пер-трекового
    /// захвата: в миксе наложенная речь неразделима, а по дорожкам — разбирается
    /// тривиально. Тесты атрибуции опираются именно на этот участок.
    ///
    /// Раскладка (мс от `T0`):
    ///
    /// ```text
    /// трек A  ████████        ██████████████
    ///         500..2000       3000..6000
    /// трек B          ████████████   ██████
    ///                 1500..3500     5000..7000
    ///                 └ наложение ┘  └ наложение ┘
    /// ```
    pub fn two_speakers_overlapping(sample_rate: u32, frame_ms: u64) -> Self {
        let tracks = vec![
            ScriptedTrack {
                track_id: "track-a".into(),
                idx: 0,
                start_offset_ms: 0,
                end_offset_ms: 8_000,
                speech: vec![SpeechInterval::new(500, 2_000), SpeechInterval::new(3_000, 6_000)],
                tone_hz: 220.0,
            },
            ScriptedTrack {
                track_id: "track-b".into(),
                idx: 1,
                start_offset_ms: 0,
                end_offset_ms: 8_000,
                speech: vec![SpeechInterval::new(1_500, 3_500), SpeechInterval::new(5_000, 7_000)],
                tone_hz: 440.0,
            },
        ];
        Self::from_tracks(&tracks, sample_rate, frame_ms, 8_000)
    }

    /// Сценарий «участник ушёл и пришёл новый»: трек кончается, следующий
    /// получает НОВЫЙ индекс. Переиспользуй мы индекс — звук двух разных людей
    /// склеился бы в один файл.
    pub fn participant_leaves_and_another_joins(sample_rate: u32, frame_ms: u64) -> Self {
        let tracks = vec![
            ScriptedTrack {
                track_id: "track-a".into(),
                idx: 0,
                start_offset_ms: 0,
                end_offset_ms: 2_000,
                speech: vec![SpeechInterval::new(200, 1_800)],
                tone_hz: 220.0,
            },
            ScriptedTrack {
                track_id: "track-b".into(),
                idx: 1,
                // Renegotiation: трек начинается ПОЗЖЕ нуля на общей оси, и это
                // же значение потом станет `adelay` в ffmpeg.
                start_offset_ms: 2_500,
                end_offset_ms: 5_000,
                speech: vec![SpeechInterval::new(3_000, 4_500)],
                tone_hz: 440.0,
            },
        ];
        Self::from_tracks(&tracks, sample_rate, frame_ms, 5_000)
    }

    /// Собирает сценарий из описания дорожек.
    ///
    /// Порядок шагов: сначала жизненный цикл до `recording_started`, затем
    /// кадры и события В ХРОНОЛОГИЧЕСКОМ порядке по всем дорожкам сразу —
    /// именно так их видит рекордер живой встречи.
    pub fn from_tracks(
        tracks: &[ScriptedTrack],
        sample_rate: u32,
        frame_ms: u64,
        total_ms: u64,
    ) -> Self {
        let mut steps = vec![
            ScriptStep::Event(MeetEvent::Queued),
            ScriptStep::Event(MeetEvent::Launching),
            ScriptStep::Event(MeetEvent::Joined),
            // Фиксированное «время старта»: сценарий обязан быть
            // воспроизводимым, а `Utc::now()` сделал бы поток разным при каждом
            // прогоне.
            ScriptStep::Event(MeetEvent::RecordingStarted {
                t0_unix_ms: 1_700_000_000_000,
            }),
            ScriptStep::Event(MeetEvent::CapturePathDetected {
                path: "rtcpeerconnection".into(),
            }),
        ];

        // Расписание событий по времени: (t_ms, порядковый ключ, событие).
        // Порядковый ключ нужен для устойчивой сортировки — иначе два события
        // на одной миллисекунде менялись бы местами от прогона к прогону.
        let mut timeline: Vec<(u64, usize, MeetEvent)> = Vec::new();
        for (n, t) in tracks.iter().enumerate() {
            timeline.push((
                t.start_offset_ms,
                n * 4,
                MeetEvent::TrackAdded {
                    track_id: t.track_id.clone(),
                    idx: t.idx,
                    start_offset_ms: t.start_offset_ms,
                },
            ));
            for (k, sp) in t.speech.iter().enumerate() {
                timeline.push((
                    sp.start_ms,
                    n * 4 + 1 + k * 100,
                    MeetEvent::SpeechStarted {
                        track_id: t.track_id.clone(),
                        at_ms: sp.start_ms,
                    },
                ));
                timeline.push((
                    sp.end_ms,
                    n * 4 + 2 + k * 100,
                    MeetEvent::SpeechEnded {
                        track_id: t.track_id.clone(),
                        at_ms: sp.end_ms,
                    },
                ));
            }
            timeline.push((
                t.end_offset_ms,
                n * 4 + 3,
                MeetEvent::TrackEnded {
                    track_id: t.track_id.clone(),
                    idx: t.idx,
                    end_offset_ms: t.end_offset_ms,
                },
            ));
        }
        timeline.sort_by_key(|(t, k, _)| (*t, *k));

        // Кадры: по всем дорожкам, шагом `frame_ms`, в хронологическом порядке.
        let samples_per_frame = (u64::from(sample_rate) * frame_ms / 1000) as usize;
        let mut frames: Vec<(u64, usize, AudioFrame)> = Vec::new();
        for (n, t) in tracks.iter().enumerate() {
            let mut at = t.start_offset_ms;
            while at + frame_ms <= t.end_offset_ms {
                let offset_samples = u64::from(sample_rate) * at / 1000;
                let pcm = render_frame(t, at, frame_ms, sample_rate, samples_per_frame);
                frames.push((at, n, AudioFrame::new(t.track_id.clone(), offset_samples, sample_rate, pcm)));
                at += frame_ms;
            }
        }
        frames.sort_by_key(|(t, n, _)| (*t, *n));

        // Слияние двух отсортированных потоков: событие на миллисекунде `t`
        // идёт РАНЬШЕ кадра, начинающегося в тот же момент, — так рекордер
        // узнаёт о треке до его первого кадра.
        let (mut ei, mut fi) = (0usize, 0usize);
        while ei < timeline.len() || fi < frames.len() {
            let take_event = match (timeline.get(ei), frames.get(fi)) {
                (Some((et, _, _)), Some((ft, _, _))) => et <= ft,
                (Some(_), None) => true,
                _ => false,
            };
            if take_event {
                steps.push(ScriptStep::Event(timeline[ei].2.clone()));
                ei += 1;
            } else {
                steps.push(ScriptStep::Audio(frames[fi].2.clone()));
                fi += 1;
            }
        }

        steps.push(ScriptStep::Event(MeetEvent::RecordingStopped {
            reason: StopReason::AllTracksEnded,
            at_ms: total_ms,
        }));

        let participants: Vec<ParticipantInfo> = tracks
            .iter()
            .enumerate()
            .map(|(n, _)| ParticipantInfo {
                platform_id: Some(format!("p{n}")),
                name: format!("Участник {}", n + 1),
                speaking: false,
            })
            .collect();

        Self {
            steps,
            observation: Observation {
                at_ms: 0,
                active_speaker: participants.first().map(|p| p.name.clone()),
                tracks: TrackObservation {
                    audio_tracks: tracks.len(),
                    // Бот тоже участник, но своего аудиотрека не присылает.
                    participants: participants.len() + 1,
                    mid_stable: true,
                    multi_csrc: false,
                    since_join_ms: total_ms,
                },
                participants,
            },
            ..Self::default()
        }
    }

    /// Все события сценария по порядку — удобно для сверки контракта порядка.
    pub fn events(&self) -> Vec<MeetEvent> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                ScriptStep::Event(e) => Some(e.clone()),
                ScriptStep::Audio(_) => None,
            })
            .collect()
    }

    /// Все кадры сценария по порядку.
    pub fn frames(&self) -> Vec<AudioFrame> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                ScriptStep::Audio(f) => Some(f.clone()),
                ScriptStep::Event(_) => None,
            })
            .collect()
    }
}

/// Считает сэмплы одного кадра: тон во время речи, тишина вне её.
///
/// Целочисленная фаза, а не `sin` от накопленного `f64`: накопление ошибки
/// давало бы разный хвост при разной длине кадра, и «байт в байт одинаково»
/// перестало бы выполняться.
fn render_frame(
    track: &ScriptedTrack,
    at_ms: u64,
    frame_ms: u64,
    sample_rate: u32,
    samples: usize,
) -> Vec<i16> {
    let speaking = track.speech.iter().any(|sp| {
        // Кадр считается речевым, если его середина попадает в интервал:
        // граничный кадр иначе достался бы обоим соседям.
        sp.contains(at_ms + frame_ms / 2)
    });
    if !speaking {
        return vec![0i16; samples];
    }
    let base = u64::from(sample_rate) * at_ms / 1000;
    (0..samples)
        .map(|i| {
            let n = base + i as u64;
            let phase = 2.0 * std::f32::consts::PI * track.tone_hz * (n as f32) / sample_rate as f32;
            (phase.sin() * 12_000.0) as i16
        })
        .collect()
}

/// Действие, которое бот совершил над страницей.
///
/// Ведётся ради одного ключевого теста: **в ассистированном режиме бот не
/// делает НИ ОДНОГО клика до подтверждения человеком.** Проверить это можно
/// только счётчиком, а не чтением кода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotAction {
    /// Клик по элементу входа: «Продолжить в браузере», «Войти», «Присоединиться».
    Click(String),
    /// Ввод имени бота.
    Type(String),
    /// Микрофон и камера.
    SetMedia { mic: bool, cam: bool },
    /// Показ окна человеку.
    ShowWindow(bool),
    Screenshot,
    /// Подтверждение человека «я внутри — пиши».
    HumanConfirmed,
    Stop(StopReason),
}

/// Бот по сценарию.
pub struct ScriptedBot {
    // Arc: проигрывание уходит в отдельную задачу и обязано быть `'static`, а
    // сценарий на восемь секунд двух дорожек это полмегабайта сэмплов —
    // копировать его на каждый прогон незачем.
    script: std::sync::Arc<BotScript>,
    actions: Mutex<Vec<BotAction>>,
    started: AtomicBool,
    human_ready: AtomicBool,
    stopped: AtomicBool,
    /// Отложенный поток шагов, если вход требует человека.
    pending: Mutex<Option<PendingPlayback>>,
}

struct PendingPlayback {
    events: mpsc::UnboundedSender<MeetEvent>,
    audio: mpsc::Sender<AudioFrame>,
}

impl std::fmt::Debug for ScriptedBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedBot")
            .field("steps", &self.script.steps.len())
            .field("join", &self.script.join)
            .field("started", &self.started.load(Ordering::SeqCst))
            .finish()
    }
}

impl ScriptedBot {
    pub fn new(script: BotScript) -> Self {
        Self {
            script: std::sync::Arc::new(script),
            actions: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            human_ready: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            pending: Mutex::new(None),
        }
    }

    /// Бот, который требует человека на входе.
    pub fn needing_human(kind: &str) -> Self {
        Self::new(BotScript {
            join: JoinOutcome::NeedsHuman(kind.to_string()),
            ..BotScript::two_speakers_overlapping(16_000, 20)
        })
    }

    /// Что бот сделал со страницей, по порядку.
    pub fn actions(&self) -> Vec<BotAction> {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Сколько раз бот кликал. Ноль до подтверждения — инвариант
    /// ассистированного режима.
    pub fn click_count(&self) -> usize {
        self.actions()
            .iter()
            .filter(|a| matches!(a, BotAction::Click(_) | BotAction::Type(_)))
            .count()
    }

    /// Сценарий, по которому бот играет.
    pub fn script(&self) -> &BotScript {
        &self.script
    }

    fn note(&self, action: BotAction) {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }

    /// Проигрывает шаги в каналы ОТДЕЛЬНОЙ ЗАДАЧЕЙ.
    ///
    /// Не спит: сценарий уже несёт метки времени, и ждать реального времени
    /// значило бы превратить восьмисекундную встречу в восьмисекундный тест.
    ///
    /// Проигрывание обязано быть задачей, а не телом `start`: канал звука
    /// ОГРАНИЧЕН ([`AUDIO_CHANNEL_CAPACITY`]), а сценарий на восемь секунд
    /// двух дорожек — восемьсот кадров. Играй мы внутри `start`, отправка
    /// упёрлась бы в заполненный канал раньше, чем вызывающий получил бы
    /// приёмник, и встали бы оба. Ровно так же ведёт себя живой бот: кадры
    /// идут из задачи CDP, а backpressure тормозит её, а не рекордер.
    fn spawn_play(
        script: std::sync::Arc<BotScript>,
        events: mpsc::UnboundedSender<MeetEvent>,
        audio: mpsc::Sender<AudioFrame>,
    ) {
        tokio::spawn(async move {
            let delay = script.playback_delay_ms;
            for step in script.steps.iter() {
                match step {
                    ScriptStep::Event(e) => {
                        if events.send(e.clone()).is_err() {
                            return;
                        }
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                    }
                    ScriptStep::Audio(f) => {
                        if audio.send(f.clone()).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
}

#[async_trait]
impl MeetBot for ScriptedBot {
    async fn start(&self, mode: JoinMode) -> Result<BotStreams, MeetError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(MeetError::Internal(
                "ScriptedBot уже запущен: каналы отдаются один раз".to_string(),
            ));
        }
        let (etx, erx) = mpsc::unbounded_channel();
        let (atx, arx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);

        match &self.script.join {
            JoinOutcome::Fail { code, message } => {
                let _ = etx.send(MeetEvent::Queued);
                let _ = etx.send(MeetEvent::Launching);
                let _ = etx.send(MeetEvent::Failed {
                    code: code.clone(),
                    message: message.clone(),
                });
            }
            JoinOutcome::NeedsHuman(kind) => {
                let _ = etx.send(MeetEvent::Queued);
                let _ = etx.send(MeetEvent::Launching);
                let _ = etx.send(MeetEvent::NeedsHuman { kind: kind.clone() });
                // Ассистированный вход: сценарий ЖДЁТ человека. Ни одного
                // клика до подтверждения.
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(PendingPlayback { events: etx, audio: atx });
            }
            JoinOutcome::Ok => {
                if mode == JoinMode::Normal {
                    // Нормальный режим кликает сам: интерстишал, имя, вход.
                    self.note(BotAction::Click("continue-in-browser".into()));
                    self.note(BotAction::Type("F.O.R.G.E.".into()));
                    self.note(BotAction::Click("join".into()));
                } else {
                    // Ассистированный: окно показано, дальше — человек.
                    self.note(BotAction::ShowWindow(true));
                    *self.pending.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(PendingPlayback { events: etx, audio: atx });
                    return Ok(BotStreams { events: erx, audio: arx });
                }
                Self::spawn_play(self.script.clone(), etx, atx);
            }
        }

        Ok(BotStreams { events: erx, audio: arx })
    }

    async fn confirm_human_ready(&self) -> Result<(), MeetError> {
        // Повторное подтверждение — не ошибка: фронт не обязан знать режим
        // встречи, чтобы не нажать лишнюю кнопку.
        if self.human_ready.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.note(BotAction::HumanConfirmed);
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(p) = pending {
            Self::spawn_play(self.script.clone(), p.events, p.audio);
        }
        Ok(())
    }

    async fn set_media(&self, mic: bool, cam: bool) -> Result<(), MeetError> {
        self.note(BotAction::SetMedia { mic, cam });
        Ok(())
    }

    async fn observe(&self) -> Result<Observation, MeetError> {
        Ok(self.script.observation.clone())
    }

    async fn show_window(&self, visible: bool) -> Result<WindowReport, MeetError> {
        self.note(BotAction::ShowWindow(visible));
        let (left, top, width, height) = if visible {
            self.script.visible_bounds
        } else {
            self.script.hidden_bounds
        };
        Ok(WindowReport {
            visible,
            left,
            top,
            width,
            height,
        })
    }

    async fn screenshot(&self) -> Result<Vec<u8>, MeetError> {
        self.note(BotAction::Screenshot);
        Ok(self.script.screenshot.clone())
    }

    async fn stop(&self, reason: StopReason) -> Result<(), MeetError> {
        // Идемпотентность: сюда приходят и по команде пользователя, и по
        // автовыходу, и из graceful shutdown — все три могут совпасть.
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.note(BotAction::Stop(reason));
        Ok(())
    }
}

/// Режим SFU для сценария — удобная обёртка над [`TrackMode`], чтобы тесты
/// мок-страницы читались.
impl BotScript {
    /// Подменяет наблюдение так, чтобы детект дал нужный режим.
    pub fn with_track_mode(mut self, mode: TrackMode) -> Self {
        self.observation.tracks = match mode {
            TrackMode::PerParticipant => TrackObservation {
                audio_tracks: 3,
                participants: 4,
                mid_stable: true,
                multi_csrc: false,
                since_join_ms: 60_000,
            },
            TrackMode::Slots => TrackObservation {
                audio_tracks: 3,
                participants: 9,
                mid_stable: true,
                multi_csrc: false,
                since_join_ms: 60_000,
            },
            TrackMode::Mixed => TrackObservation {
                audio_tracks: 1,
                participants: 5,
                mid_stable: true,
                multi_csrc: true,
                since_join_ms: 60_000,
            },
            TrackMode::Unknown => TrackObservation::default(),
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::event::validate_order;

    /// Читает ОБА канала одновременно — ровно как настоящий рекордер.
    ///
    /// Последовательное чтение здесь не работает и это не мелочь: канал звука
    /// ограничен, восемьсот кадров сценария в него не влезают, и «сначала все
    /// события, потом все кадры» встало бы намертво. Тест обязан ходить теми
    /// же тропами, что и боевой код.
    async fn drain(streams: BotStreams) -> (Vec<MeetEvent>, Vec<AudioFrame>) {
        let BotStreams { mut events, mut audio } = streams;
        let e = tokio::spawn(async move {
            let mut v = Vec::new();
            while let Some(x) = events.recv().await {
                v.push(x);
            }
            v
        });
        let a = tokio::spawn(async move {
            let mut v = Vec::new();
            while let Some(x) = audio.recv().await {
                v.push(x);
            }
            v
        });
        (e.await.unwrap(), a.await.unwrap())
    }

    /// ГЛАВНОЕ свойство: повтор даёт БАЙТ В БАЙТ тот же поток.
    #[tokio::test]
    async fn the_same_script_replays_byte_for_byte() {
        let script = BotScript::two_speakers_overlapping(16_000, 20);

        let first = {
            let bot = ScriptedBot::new(script.clone());
            drain(bot.start(JoinMode::Normal).await.unwrap()).await
        };
        let second = {
            let bot = ScriptedBot::new(script.clone());
            drain(bot.start(JoinMode::Normal).await.unwrap()).await
        };

        assert_eq!(first.0, second.0, "поток событий разошёлся между прогонами");
        assert_eq!(first.1.len(), second.1.len(), "число кадров разошлось");
        for (a, b) in first.1.iter().zip(second.1.iter()) {
            assert_eq!(a.to_le_bytes(), b.to_le_bytes(), "кадр {} разошёлся", a.track_id);
        }
    }

    /// Сценарий обязан соблюдать тот же контракт порядка, что и живой бот.
    #[tokio::test]
    async fn the_scripted_stream_obeys_the_order_contract() {
        let script = BotScript::two_speakers_overlapping(16_000, 20);
        validate_order(&script.events()).unwrap();

        let script = BotScript::participant_leaves_and_another_joins(16_000, 20);
        validate_order(&script.events()).unwrap();
    }

    /// Перебивание — то, ради чего вообще делается пер-трековый захват.
    #[tokio::test]
    async fn two_speakers_really_overlap() {
        let script = BotScript::two_speakers_overlapping(16_000, 20);
        let bot = ScriptedBot::new(script);
        let (events, frames) = drain(bot.start(JoinMode::Normal).await.unwrap()).await;

        // Участок 1500..2000 мс: говорят оба.
        let both_at = |t: u64| {
            let a = frames.iter().find(|f| f.track_id == "track-a" && f.start_ms() == t);
            let b = frames.iter().find(|f| f.track_id == "track-b" && f.start_ms() == t);
            (a.map(AudioFrame::rms).unwrap_or(0.0), b.map(AudioFrame::rms).unwrap_or(0.0))
        };
        let (a, b) = both_at(1_700);
        assert!(a > 0.1 && b > 0.1, "наложение речи не воспроизвелось: a={a}, b={b}");

        // А в 2500 мс — только B.
        let (a, b) = both_at(2_500);
        assert!(a < 0.01, "трек A обязан молчать после 2000 мс, rms={a}");
        assert!(b > 0.1, "трек B обязан говорить, rms={b}");

        // События речи объявлены на обоих треках.
        let starts: Vec<&MeetEvent> = events
            .iter()
            .filter(|e| matches!(e, MeetEvent::SpeechStarted { .. }))
            .collect();
        assert_eq!(starts.len(), 4, "два интервала на двух треках");
    }

    /// Индекс не переиспользуется: ушедший участник не отдаёт свой номер
    /// пришедшему, иначе звук двоих склеился бы в один файл.
    #[tokio::test]
    async fn a_leaving_participant_does_not_hand_over_its_index() {
        let script = BotScript::participant_leaves_and_another_joins(16_000, 20);
        let bot = ScriptedBot::new(script);
        let (events, frames) = drain(bot.start(JoinMode::Normal).await.unwrap()).await;

        let added: Vec<(String, usize, u64)> = events
            .iter()
            .filter_map(|e| match e {
                MeetEvent::TrackAdded { track_id, idx, start_offset_ms } => {
                    Some((track_id.clone(), *idx, *start_offset_ms))
                }
                _ => None,
            })
            .collect();
        assert_eq!(added.len(), 2);
        assert_ne!(added[0].1, added[1].1, "индекс переиспользован");
        assert_eq!(added[1].2, 2_500, "renegotiation даёт ненулевое смещение");

        // И кадры второго трека тоже начинаются со смещения, а не с нуля.
        let first_b = frames.iter().find(|f| f.track_id == "track-b").unwrap();
        assert_eq!(first_b.start_ms(), 2_500);
        assert_eq!(first_b.offset_samples, 16_000 * 2_500 / 1_000);
    }

    /// ИНВАРИАНТ АССИСТИРОВАННОГО ВХОДА: ни одного клика до подтверждения.
    #[tokio::test]
    async fn assisted_mode_clicks_nothing_until_a_human_says_so() {
        let bot = ScriptedBot::new(BotScript::two_speakers_overlapping(16_000, 20));
        let mut streams = bot.start(JoinMode::Assisted).await.unwrap();

        assert_eq!(bot.click_count(), 0, "бот кликнул до подтверждения: {:?}", bot.actions());
        assert!(streams.events.try_recv().is_err(), "запись пошла без человека");
        assert!(
            bot.actions().contains(&BotAction::ShowWindow(true)),
            "ассистированный вход обязан показать окно"
        );

        bot.confirm_human_ready().await.unwrap();
        assert_eq!(bot.click_count(), 0, "подтверждение не должно кликать за человека");

        let (got, frames) = drain(streams).await;
        assert!(!got.is_empty(), "после подтверждения запись обязана пойти");
        assert!(!frames.is_empty(), "звук обязан пойти немедленно: инжект уже стоял");
        validate_order(&got).unwrap();
    }

    /// В обычном режиме бот кликает сам — и это видно.
    #[tokio::test]
    async fn normal_mode_does_click() {
        let bot = ScriptedBot::new(BotScript::two_speakers_overlapping(16_000, 20));
        bot.start(JoinMode::Normal).await.unwrap();
        assert!(bot.click_count() >= 2, "{:?}", bot.actions());
    }

    /// Сценарий «нужен человек» отдаёт needs_human и доступный скриншот.
    #[tokio::test]
    async fn the_needs_human_scenario_reports_the_reason_and_a_screenshot() {
        let bot = ScriptedBot::needing_human("captcha");
        let mut streams = bot.start(JoinMode::Normal).await.unwrap();

        let mut kinds = Vec::new();
        // Три кадра входа уже отправлены синхронно внутри `start`, поэтому
        // читать их можно без ожидания; дальше поток стоит и ждёт человека.
        while let Ok(e) = streams.events.try_recv() {
            if let MeetEvent::NeedsHuman { kind } = &e {
                kinds.push(kind.clone());
            }
        }
        assert_eq!(kinds, vec!["captcha"], "причина обязана быть названа");

        let png = bot.screenshot().await.unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], "не PNG");

        // Ни одной ветки, решающей капчу: бот только зовёт человека.
        assert_eq!(bot.click_count(), 0);
    }

    #[tokio::test]
    async fn the_streams_are_handed_out_exactly_once() {
        let bot = ScriptedBot::new(BotScript::default());
        assert!(bot.start(JoinMode::Normal).await.is_ok());
        assert!(bot.start(JoinMode::Normal).await.is_err(), "каналы отданы дважды");
    }

    #[tokio::test]
    async fn stopping_twice_is_not_an_error() {
        let bot = ScriptedBot::new(BotScript::default());
        bot.stop(StopReason::UserStop).await.unwrap();
        bot.stop(StopReason::Shutdown).await.unwrap();
        // Записано ровно одно действие: повторный стоп не выдумывает событий.
        assert_eq!(
            bot.actions()
                .iter()
                .filter(|a| matches!(a, BotAction::Stop(_)))
                .count(),
            1
        );
    }

    /// Показ окна возвращает ФАКТИЧЕСКИЕ границы, а не запрошенные: на macOS
    /// оконный сервер подрезает координаты (измерено в Ф1).
    #[tokio::test]
    async fn hiding_the_window_reports_what_actually_happened() {
        let bot = ScriptedBot::new(BotScript::default());
        let hidden = bot.show_window(false).await.unwrap();
        assert_eq!(hidden.left, -930, "запрошенный -4000 подрезается системой");
        let shown = bot.show_window(true).await.unwrap();
        assert!(shown.left >= 0 && shown.visible);
    }

    /// Наблюдение бота скармливается детекту режима без переходников.
    #[tokio::test]
    async fn the_observation_feeds_the_mode_detector_directly() {
        for want in [TrackMode::PerParticipant, TrackMode::Slots, TrackMode::Mixed] {
            let script = BotScript::two_speakers_overlapping(16_000, 20).with_track_mode(want);
            let bot = ScriptedBot::new(script);
            let obs = bot.observe().await.unwrap();
            assert_eq!(
                super::super::detect_track_mode(&obs.tracks, super::super::mode::DEFAULT_MODE_WARMUP_MS),
                want
            );
        }
    }
}
