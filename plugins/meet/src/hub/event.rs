//! События встречи и приёмник [`MeetSink`].
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Почему у трейта ОДИН метод, а не двенадцать
//!
//! У `AnalysisSink` из `forge-chat-analysis` четыре метода, и это оправдано:
//! там у каждого вызова СВОЙ контракт кратности («ровно один раз», «много
//! раз», «последним»), и разные методы делают эти контракты видимыми в
//! сигнатуре.
//!
//! Здесь ситуация другая. Событий уже больше десятка, и их число растёт с
//! каждой фазой: Ф4 добавит имена участников и конец встречи, Ф5 — прогресс
//! расшифровки. При методе на событие каждая новая фаза заставляла бы дописать
//! пустую заглушку в КАЖДУЮ реализацию — в `ScriptedBot`, в тестовых синках, в
//! рекордере. Один метод `emit` с перечислением превращает это в добавление
//! варианта, который существующие реализации обрабатывают ветвью `_ => {}`.
//!
//! Контракт порядка при этом никуда не девается: он записан ниже и проверен
//! [`validate_order`], которую зовёт тест, а не читатель.
//!
//! # `emit` СИНХРОНЕН
//!
//! Его зовут из горячего цикла разбора аудиокадров: двадцатимиллисекундный
//! кадр на трек, пять треков — 250 вызовов в секунду. `async fn` здесь означал
//! бы точку отмены внутри цикла, который обязан дописать кадр на диск целиком.
//! Реализация, которой нужна асинхронность (запись в базу, вещание в WS),
//! кладёт событие в неограниченную очередь и разбирает её отдельной задачей —
//! см. [`super::sink::EventFanout`].
//!
//! # КОНТРАКТ ПОРЯДКА
//!
//! ```text
//! Queued
//!   → Launching
//!     → [NeedsHuman{kind} | WaitingAdmit]*   ← капчу мы НЕ обходим, ждём человека;
//!                                              лобби — просто ждём площадку
//!       → Joined
//!         → RecordingStarted{t0}     ← отсюда и только отсюда идёт ось времени
//!           → (TrackAdded | TrackEnded | SpeechStarted | SpeechEnded |
//!              ParticipantsChanged | ActiveSpeaker | TrackModeDetected |
//!              CapturePathDetected | FramesDropped)*
//!             → RecordingStopped{reason}
//! ```
//!
//! Плюс [`MeetEvent::Failed`], законный на любой стадии до
//! `RecordingStopped`. [`validate_order`] проверяет ровно это.
//!
//! Смысл контракта практический: [`MeetEvent::RecordingStarted`] несёт `T0`, и
//! любое событие с полем `at_ms` до него не к чему привязать. Событие о треке
//! после `RecordingStopped` означало бы дозапись в уже закрытый WAV.

use serde::{Deserialize, Serialize};

use crate::bot::mode::TrackMode;

/// Почему запись закончилась.
///
/// Список закрытый и общий для всех платформ: у детекта конца встречи
/// НЕСКОЛЬКО независимых сигналов, и без указания, какой именно сработал, через
/// сутки невозможно ответить, почему бот вышел на десятой минуте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// `POST /api/meet/meetings/:id/stop` — «встреча кончилась, обработай».
    UserStop,
    /// `POST /api/meet/meetings/:id/cancel` — «прерви и выброси».
    UserCancel,
    /// Все удалённые треки в состоянии `ended`.
    AllTracksEnded,
    /// Бот остался один (`alone_timeout_secs`).
    Alone,
    /// Тишина при не более чем одном собеседнике (`silence_timeout_secs`).
    Silence,
    /// DOM-маркер конца встречи либо пост-звонковый URL.
    DomEnded,
    /// Потолок длительности (`max_meeting_secs`).
    MaxDuration,
    /// Target упал: Chrome закрыли, вкладка умерла.
    BrowserGone,
    /// Плагин останавливается (graceful shutdown devforge).
    Shutdown,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserStop => "user_stop",
            Self::UserCancel => "user_cancel",
            Self::AllTracksEnded => "all_tracks_ended",
            Self::Alone => "alone",
            Self::Silence => "silence",
            Self::DomEnded => "dom_ended",
            Self::MaxDuration => "max_duration",
            Self::BrowserGone => "browser_gone",
            Self::Shutdown => "shutdown",
        }
    }

    /// Сохраняются ли файлы. `cancel` — единственный случай, когда нет.
    pub fn keeps_files(self) -> bool {
        !matches!(self, Self::UserCancel)
    }
}

/// Участник встречи глазами бота (DOM-проба или msid трека).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantInfo {
    /// Идентификатор в терминах платформы, если он есть. `None` — участник
    /// известен только по видимому имени.
    pub platform_id: Option<String>,
    /// Видимое имя. Пусто не бывает: деградация — «Участник N».
    pub name: String,
    /// Считает ли платформа этого участника говорящим прямо сейчас.
    pub speaking: bool,
}

/// Событие жизненного цикла встречи.
///
/// Порядок вариантов совпадает с контрактом порядка из шапки модуля — на него
/// опирается [`MeetEvent::rank`], а через неё [`validate_order`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MeetEvent {
    /// Заявка принята, слот записи занят.
    Queued,

    /// Запускается Chrome.
    Launching,

    /// Нужен человек: капча, логин, лобби.
    ///
    /// **Капчу мы не обходим ни при каких условиях** — событие означает
    /// «покажи человеку скриншот и жди». Может повторяться: сначала логин,
    /// потом лобби.
    NeedsHuman {
        /// `captcha` | `login` | `lobby` | `unknown`.
        kind: String,
    },

    /// Бот в лобби: вход нажат, площадка ждёт решения организатора.
    ///
    /// Отдельное событие, а не разновидность [`MeetEvent::NeedsHuman`],
    /// потому что человек здесь НЕ НУЖЕН — нужно подождать. Смешай их, и
    /// интерфейс просил бы вмешательства там, где всё идёт по плану; статус
    /// `waiting_admit` существует ровно для этого случая.
    WaitingAdmit,

    /// Бот внутри встречи. Записи ещё нет.
    Joined,

    /// **Начало единственной оси времени.**
    ///
    /// Все последующие `*_ms` считаются от этого момента через
    /// `performance.now()` СТРАНИЦЫ. Поле `t0_unix_ms` существует ровно для
    /// того, чтобы человек мог соотнести запись с календарём, и НИКОГДА не
    /// участвует в арифметике таймлайна: подмешай мы в ось время бэкенда, в
    /// неё просочился бы джиттер CDP.
    RecordingStarted { t0_unix_ms: i64 },

    /// Появился аудиотрек.
    TrackAdded {
        track_id: String,
        /// Номер файла `raw/track-N.wav`. **Не переиспользуется после
        /// `TrackEnded`**: иначе звук двух разных людей склеился бы в один
        /// файл.
        idx: usize,
        /// Смещение начала трека на общей оси. Renegotiation даёт трек,
        /// который начинается позже нуля, и это же значение потом становится
        /// `adelay` в ffmpeg.
        start_offset_ms: u64,
        /// `msid` потока, в котором дорожка приехала. **Ось привязки трека к
        /// человеку на уровне WebRTC**, первая ступень каскада
        /// [`crate::bot::BindingStage::Msid`].
        ///
        /// `None` — площадка связь не раскрывает, и это норма: у Телемоста
        /// `msid` не связан с конкретным человеком вовсе (измерено Ф0).
        /// У LiveKit (vcc.airlabs.pro) он имеет вид
        /// `PA_<sid участника>|TR_<sid дорожки>` — человеческого имени там
        /// нет, но ДВЕ дорожки одного человека узнаются по общей части `PA_*`,
        /// и это ровно та устойчивость, которой не даёт корреляция.
        ///
        /// Поле необязательное и не сериализуется, когда его нет: журналы,
        /// записанные до Ф13, обязаны читаться прежним разбором.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        msid: Option<String>,
    },

    /// Трек закончился.
    TrackEnded { track_id: String, idx: usize, end_offset_ms: u64 },

    /// Начало речи в треке. Момент УЖЕ сдвинут назад на длину подтверждения
    /// VAD — иначе все интервалы систематически съезжали бы вправо.
    SpeechStarted { track_id: String, at_ms: u64 },

    /// Конец речи в треке (после hangover).
    SpeechEnded { track_id: String, at_ms: u64 },

    /// Список участников изменился (DOM-проба, Ф4).
    ParticipantsChanged { at_ms: u64, participants: Vec<ParticipantInfo> },

    /// Платформа сменила активного спикера. `None` — говорящих нет.
    ActiveSpeaker { at_ms: u64, name: Option<String> },

    /// Определён режим SFU. Приходит примерно через 20 с после входа и
    /// переоценивается: режим — величина рантайма, а не свойство платформы.
    TrackModeDetected { at_ms: u64, mode: TrackMode },

    /// Каким из трёх тапов реально пошёл звук. Уезжает в манифест полем
    /// `capture_path`.
    CapturePathDetected { path: String },

    /// Страница сообщила о потерянных кадрах (кольцевой буфер переполнился до
    /// появления биндинга). Молчаливая потеря первых секунд записи — худший
    /// исход, поэтому счётчик обязан быть виден.
    FramesDropped { total: u64 },

    /// **Со звуком что-то не так, а встреча ещё идёт.**
    ///
    /// Предупреждение, а не провал: бот продолжает писать. Существует потому,
    /// что худший исход записи — не падение, а ТИШИНА, замеченная через час,
    /// когда спасать уже нечего. Живой случай: медиа-канал площадки не
    /// поднялся (DNS медиасервера не разрешился, TURN ответил 403), сигнальный
    /// канал работал, дорожки завелись, и 63 минуты писались нули — а узналось
    /// это только из `ffmpeg` сутки спустя.
    ///
    /// Два источника, и оба нужны: расхождение «DOM показывает говорящих, VAD
    /// молчит» видно изнутри, ошибки медиа-канала из `chrome.log` — снаружи.
    /// Первый ловит любую причину глухоты, второй называет её.
    MediaWarning {
        at_ms: u64,
        /// `deaf_bot` | `media_transport`.
        code: String,
        /// Человеческое объяснение: что именно наблюдалось.
        detail: String,
    },

    /// Запись закончена.
    RecordingStopped { reason: StopReason, at_ms: u64 },

    /// Провал. Законен на любой стадии до [`MeetEvent::RecordingStopped`].
    Failed { code: String, message: String },
}

/// Ранг стадии для проверки порядка.
///
/// Отдельные числа, а не `as usize` по варианту: события середины записи
/// перемешиваются между собой свободно и обязаны иметь ОДИН ранг, иначе
/// «участники обновились после начала речи» выглядело бы нарушением.
const RANK_QUEUED: u8 = 0;
const RANK_LAUNCHING: u8 = 1;
const RANK_JOINING: u8 = 2;
const RANK_JOINED: u8 = 3;
const RANK_RECORDING: u8 = 4;
const RANK_DURING: u8 = 5;
const RANK_STOPPED: u8 = 6;

impl MeetEvent {
    /// Машиночитаемое имя события — оно же тег в JSON.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Launching => "launching",
            Self::NeedsHuman { .. } => "needs_human",
            Self::WaitingAdmit => "waiting_admit",
            Self::Joined => "joined",
            Self::RecordingStarted { .. } => "recording_started",
            Self::TrackAdded { .. } => "track_added",
            Self::TrackEnded { .. } => "track_ended",
            Self::SpeechStarted { .. } => "speech_started",
            Self::SpeechEnded { .. } => "speech_ended",
            Self::ParticipantsChanged { .. } => "participants_changed",
            Self::ActiveSpeaker { .. } => "active_speaker",
            Self::TrackModeDetected { .. } => "track_mode_detected",
            Self::CapturePathDetected { .. } => "capture_path_detected",
            Self::FramesDropped { .. } => "frames_dropped",
            Self::MediaWarning { .. } => "media_warning",
            Self::RecordingStopped { .. } => "recording_stopped",
            Self::Failed { .. } => "failed",
        }
    }

    /// Стадия, которой принадлежит событие. См. контракт порядка в шапке.
    fn rank(&self) -> u8 {
        match self {
            Self::Queued => RANK_QUEUED,
            Self::Launching => RANK_LAUNCHING,
            Self::NeedsHuman { .. } | Self::WaitingAdmit => RANK_JOINING,
            Self::Joined => RANK_JOINED,
            Self::RecordingStarted { .. } => RANK_RECORDING,
            Self::TrackAdded { .. }
            | Self::TrackEnded { .. }
            | Self::SpeechStarted { .. }
            | Self::SpeechEnded { .. }
            | Self::ParticipantsChanged { .. }
            | Self::ActiveSpeaker { .. }
            | Self::TrackModeDetected { .. }
            | Self::CapturePathDetected { .. }
            | Self::FramesDropped { .. }
            | Self::MediaWarning { .. } => RANK_DURING,
            Self::RecordingStopped { .. } => RANK_STOPPED,
            // Провал законен где угодно — ранг ему не нужен, и
            // `validate_order` его пропускает раньше, чем спросит.
            Self::Failed { .. } => RANK_DURING,
        }
    }

    /// Есть ли у события метка на общей оси времени.
    ///
    /// Нужна проверке «ни одна метка не появляется раньше `T0`»: до
    /// [`MeetEvent::RecordingStarted`] оси не существует, и число миллисекунд
    /// не к чему привязать.
    pub fn at_ms(&self) -> Option<u64> {
        match self {
            Self::TrackAdded { start_offset_ms, .. } => Some(*start_offset_ms),
            Self::TrackEnded { end_offset_ms, .. } => Some(*end_offset_ms),
            Self::SpeechStarted { at_ms, .. }
            | Self::SpeechEnded { at_ms, .. }
            | Self::ParticipantsChanged { at_ms, .. }
            | Self::ActiveSpeaker { at_ms, .. }
            | Self::TrackModeDetected { at_ms, .. }
            | Self::MediaWarning { at_ms, .. }
            | Self::RecordingStopped { at_ms, .. } => Some(*at_ms),
            _ => None,
        }
    }

    /// Терминальное ли событие: после него поток обязан кончиться.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::RecordingStopped { .. } | Self::Failed { .. })
    }
}

/// Нарушение контракта порядка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderViolation {
    /// Индекс события в потоке.
    pub index: usize,
    /// Что произошло.
    pub message: String,
}

impl std::fmt::Display for OrderViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "событие #{}: {}", self.index, self.message)
    }
}

/// Проверяет поток событий на соответствие контракту порядка.
///
/// Существует затем, чтобы контракт из док-комментария был ИСПОЛНИМЫМ, а не
/// пожеланием: её зовут тесты хаба и тест `ScriptedBot`, а Ф4 позовёт из
/// живого адаптера в debug-сборке.
///
/// Правила:
///
/// 1. ранг не убывает (стадии идут вперёд);
/// 2. [`MeetEvent::Failed`] законен где угодно, но после него ничего нет;
/// 3. после [`MeetEvent::RecordingStopped`] ничего нет;
/// 4. событие с меткой времени не может стоять раньше
///    [`MeetEvent::RecordingStarted`] — оси ещё не существует.
pub fn validate_order(events: &[MeetEvent]) -> Result<(), OrderViolation> {
    let mut rank = 0u8;
    let mut recording_started = false;
    let mut finished: Option<&'static str> = None;

    for (index, ev) in events.iter().enumerate() {
        if let Some(what) = finished {
            return Err(OrderViolation {
                index,
                message: format!("{} после {what}", ev.kind()),
            });
        }
        if let MeetEvent::Failed { .. } = ev {
            finished = Some("failed");
            continue;
        }
        if ev.rank() < rank {
            return Err(OrderViolation {
                index,
                message: format!("{} вернулось на предыдущую стадию", ev.kind()),
            });
        }
        rank = ev.rank();
        if matches!(ev, MeetEvent::RecordingStarted { .. }) {
            recording_started = true;
        }
        if ev.at_ms().is_some() && !recording_started {
            return Err(OrderViolation {
                index,
                message: format!(
                    "{} несёт метку времени до recording_started — оси ещё нет",
                    ev.kind()
                ),
            });
        }
        if matches!(ev, MeetEvent::RecordingStopped { .. }) {
            finished = Some("recording_stopped");
        }
    }
    Ok(())
}

/// Приёмник событий встречи. **Единственный метод.**
///
/// Синхронный: зовётся из горячего цикла разбора аудиокадров (см. шапку
/// модуля).
pub trait MeetSink: Send + Sync {
    /// Принимает событие. Реализация обязана возвращаться быстро и НЕ
    /// паниковать: паника здесь убила бы задачу записи вместе с недописанным
    /// WAV.
    fn emit(&self, event: MeetEvent);
}

/// Синк, который всё выбрасывает. Нужен там, где события никому не интересны
/// (пробы, юнит-тесты чистых функций).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl MeetSink for NullSink {
    fn emit(&self, _event: MeetEvent) {}
}

/// Синк, копящий события в памяти. Основа тестов: поток можно сравнить
/// целиком, а не по одному ожиданию за раз.
#[derive(Debug, Default)]
pub struct CollectingSink {
    events: std::sync::Mutex<Vec<MeetEvent>>,
}

impl CollectingSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Снимок собранного.
    pub fn events(&self) -> Vec<MeetEvent> {
        // Отравление мьютекса не повод терять уже собранные события.
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Имена событий по порядку — компактная форма для ассертов.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(MeetEvent::kind).collect()
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl MeetSink for CollectingSink {
    fn emit(&self, event: MeetEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
    }
}

/// Синк из двух: события идут в оба. Нужен, когда запись одновременно уезжает
/// в `events.ndjson` и в WS.
pub struct TeeSink<A, B>(pub A, pub B);

impl<A: MeetSink, B: MeetSink> MeetSink for TeeSink<A, B> {
    fn emit(&self, event: MeetEvent) {
        self.0.emit(event.clone());
        self.1.emit(event);
    }
}

impl<T: MeetSink + ?Sized> MeetSink for std::sync::Arc<T> {
    fn emit(&self, event: MeetEvent) {
        (**self).emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_path() -> Vec<MeetEvent> {
        vec![
            MeetEvent::Queued,
            MeetEvent::Launching,
            MeetEvent::Joined,
            MeetEvent::RecordingStarted { t0_unix_ms: 1_700_000_000_000 },
            MeetEvent::CapturePathDetected { path: "rtcpeerconnection".into() },
            MeetEvent::TrackAdded {
                track_id: "a".into(),
                idx: 0,
                start_offset_ms: 0,
                msid: None,
            },
            MeetEvent::SpeechStarted { track_id: "a".into(), at_ms: 500 },
            MeetEvent::SpeechEnded { track_id: "a".into(), at_ms: 1_500 },
            MeetEvent::TrackEnded {
                track_id: "a".into(),
                idx: 0,
                end_offset_ms: 2_000,
            },
            MeetEvent::RecordingStopped {
                reason: StopReason::UserStop,
                at_ms: 2_100,
            },
        ]
    }

    #[test]
    fn the_documented_happy_path_validates() {
        validate_order(&happy_path()).unwrap();
    }

    /// Ассистированный вход: `needs_human` может повториться (сначала логин,
    /// потом лобби) и стоит между запуском и входом.
    #[test]
    fn needs_human_may_repeat_before_joining() {
        let events = vec![
            MeetEvent::Queued,
            MeetEvent::Launching,
            MeetEvent::NeedsHuman { kind: "login".into() },
            MeetEvent::NeedsHuman { kind: "lobby".into() },
            MeetEvent::Joined,
            MeetEvent::RecordingStarted { t0_unix_ms: 1 },
            MeetEvent::RecordingStopped {
                reason: StopReason::UserStop,
                at_ms: 10,
            },
        ];
        validate_order(&events).unwrap();
    }

    #[test]
    fn stages_never_go_backwards() {
        let events = vec![
            MeetEvent::Queued,
            MeetEvent::Joined,
            MeetEvent::Launching,
        ];
        let err = validate_order(&events).unwrap_err();
        assert_eq!(err.index, 2);
        assert!(err.message.contains("launching"), "{err}");
    }

    /// Главное практическое правило: до `recording_started` оси времени нет, и
    /// метка `at_ms` не к чему привязать.
    #[test]
    fn a_timestamp_before_t0_is_a_violation() {
        let events = vec![
            MeetEvent::Queued,
            MeetEvent::Launching,
            MeetEvent::Joined,
            MeetEvent::SpeechStarted { track_id: "a".into(), at_ms: 5 },
        ];
        let err = validate_order(&events).unwrap_err();
        assert_eq!(err.index, 3);
        assert!(err.message.contains("оси ещё нет"), "{err}");
    }

    #[test]
    fn nothing_follows_the_end_of_the_recording() {
        let mut events = happy_path();
        events.push(MeetEvent::SpeechStarted {
            track_id: "a".into(),
            at_ms: 3_000,
        });
        let err = validate_order(&events).unwrap_err();
        assert!(err.message.contains("после recording_stopped"), "{err}");
    }

    #[test]
    fn failure_is_legal_anywhere_but_ends_the_stream() {
        for cut in 0..happy_path().len() - 1 {
            let mut events: Vec<MeetEvent> = happy_path().into_iter().take(cut).collect();
            events.push(MeetEvent::Failed {
                code: "chrome_unavailable".into(),
                message: "нет Chrome".into(),
            });
            validate_order(&events).unwrap_or_else(|e| panic!("обрыв на {cut}: {e}"));

            events.push(MeetEvent::Queued);
            assert!(validate_order(&events).is_err(), "после failed ничего нет");
        }
    }

    /// Имена событий — контракт с `events.ndjson` и с фронтом: переименование
    /// варианта не имеет права пройти молча.
    #[test]
    fn event_names_are_frozen_and_match_the_json_tag() {
        let all = vec![
            MeetEvent::Queued,
            MeetEvent::Launching,
            MeetEvent::NeedsHuman { kind: String::new() },
            MeetEvent::WaitingAdmit,
            MeetEvent::Joined,
            MeetEvent::RecordingStarted { t0_unix_ms: 0 },
            MeetEvent::TrackAdded {
                track_id: String::new(),
                idx: 0,
                start_offset_ms: 0,
                msid: None,
            },
            MeetEvent::TrackEnded {
                track_id: String::new(),
                idx: 0,
                end_offset_ms: 0,
            },
            MeetEvent::SpeechStarted { track_id: String::new(), at_ms: 0 },
            MeetEvent::SpeechEnded { track_id: String::new(), at_ms: 0 },
            MeetEvent::ParticipantsChanged { at_ms: 0, participants: vec![] },
            MeetEvent::ActiveSpeaker { at_ms: 0, name: None },
            MeetEvent::TrackModeDetected {
                at_ms: 0,
                mode: TrackMode::PerParticipant,
            },
            MeetEvent::CapturePathDetected { path: String::new() },
            MeetEvent::FramesDropped { total: 0 },
            MeetEvent::RecordingStopped {
                reason: StopReason::UserStop,
                at_ms: 0,
            },
            MeetEvent::Failed {
                code: String::new(),
                message: String::new(),
            },
        ];
        assert_eq!(all.len(), 17, "добавили событие — допишите его в контракт порядка");

        let mut seen = std::collections::HashSet::new();
        for ev in &all {
            assert!(seen.insert(ev.kind()), "дубль имени {}", ev.kind());
            let json = serde_json::to_value(ev).unwrap();
            assert_eq!(
                json["event"].as_str(),
                Some(ev.kind()),
                "тег JSON разошёлся с kind(): {json}"
            );
            // Круговой ход: `events.ndjson` читается обратно при разборе
            // прерванной записи.
            let back: MeetEvent = serde_json::from_value(json).unwrap();
            assert_eq!(&back, ev);
        }
    }

    #[test]
    fn cancel_is_the_only_reason_that_throws_files_away() {
        for r in [
            StopReason::UserStop,
            StopReason::AllTracksEnded,
            StopReason::Alone,
            StopReason::Silence,
            StopReason::DomEnded,
            StopReason::MaxDuration,
            StopReason::BrowserGone,
            StopReason::Shutdown,
        ] {
            assert!(r.keeps_files(), "{}", r.as_str());
        }
        assert!(!StopReason::UserCancel.keeps_files());
    }

    /// `msid` УЕЗЖАЕТ В ЖУРНАЛ ПОД ИМЕНЕМ `msid` — и не появляется, когда его
    /// нет.
    ///
    /// Имя ключа заморожено не из вкуса: разбор журнала
    /// (`pipeline::run::timeline_from_journal`) ищет в данных события ровно
    /// `msid`, и переименование поля тихо оборвало бы первую ступень каскада
    /// привязки. Отсутствие ключа при `None` так же существенно: журналы,
    /// записанные до Ф13, обязаны читаться прежним разбором, а `"msid": null`
    /// в них не встречается.
    #[test]
    fn the_msid_reaches_the_journal_under_its_frozen_name() {
        let with = MeetEvent::TrackAdded {
            track_id: "a".into(),
            idx: 0,
            start_offset_ms: 0,
            msid: Some("PA_alice|TR_mic".into()),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["msid"], "PA_alice|TR_mic");
        assert_eq!(json["event"], "track_added");

        let without = MeetEvent::TrackAdded {
            track_id: "a".into(),
            idx: 0,
            start_offset_ms: 0,
            msid: None,
        };
        let json = serde_json::to_value(&without).unwrap();
        assert!(json.get("msid").is_none(), "пустая связь не имеет права ехать в журнал");

        // И обратно: событие без ключа читается, как читалось до Ф13.
        let old = serde_json::json!({
            "event": "track_added", "track_id": "a", "idx": 0, "start_offset_ms": 0
        });
        assert_eq!(serde_json::from_value::<MeetEvent>(old).unwrap(), without);
    }

    #[test]
    fn the_collecting_sink_keeps_order() {
        let sink = CollectingSink::new();
        for ev in happy_path() {
            sink.emit(ev);
        }
        assert_eq!(sink.len(), 10);
        assert_eq!(sink.kinds()[0], "queued");
        assert_eq!(sink.kinds()[9], "recording_stopped");
        validate_order(&sink.events()).unwrap();
    }

    #[test]
    fn a_tee_delivers_to_both_halves() {
        let a = std::sync::Arc::new(CollectingSink::new());
        let b = std::sync::Arc::new(CollectingSink::new());
        let tee = TeeSink(a.clone(), b.clone());
        tee.emit(MeetEvent::Queued);
        tee.emit(MeetEvent::Launching);
        assert_eq!(a.kinds(), vec!["queued", "launching"]);
        assert_eq!(b.kinds(), a.kinds());
    }
}
