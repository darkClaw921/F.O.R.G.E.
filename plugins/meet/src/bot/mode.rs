//! Режим раздачи звука у SFU: можно ли считать трек человеком.
//!
//! Модуль НЕ импортирует `axum` и `http` и не делает ввода-вывода.
//!
//! # Почему это величина РАНТАЙМА, а не свойство платформы
//!
//! План исходил из того, что режим — характеристика площадки: у Телемоста
//! трек на участника, у Meet слоты. Живая проба Ф0 показала обратное:
//! **Телемост тоже раздаёт слоты** — при трёх участниках приходит фиксированное
//! число дорожек, и `msid` не связан с конкретным человеком. То есть режим
//! оказался переменной, и определять его надо у той встречи, в которой бот
//! сидит прямо сейчас.
//!
//! Практическое следствие одно и оно жёсткое: в режиме, отличном от
//! [`TrackMode::PerParticipant`], **трекам верить нельзя**, и имя говорящего
//! берётся только из DOM. Пер-трековая расшифровка при этом остаётся
//! осмысленной (звук по дорожкам всё равно разделён), а вот подпись «это
//! говорил Иванов» — уже нет.

use serde::{Deserialize, Serialize};

/// Как SFU раздаёт удалённый звук.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackMode {
    /// Треков примерно столько же, сколько участников, и `mid` стабилен.
    /// Трек равен человеку — пер-трековая атрибуция работает как задумано.
    PerParticipant,
    /// Треков фиксированно 3–4 при растущем числе участников: SFU
    /// перекладывает активных говорящих в постоянный набор дорожек.
    /// **Трекам не верим**, спикер только из DOM.
    Slots,
    /// Один трек при более чем двух участниках: звук смикширован на сервере.
    /// Прямой признак — несколько CSRC с ненулевым уровнем в
    /// `getContributingSources()`.
    Mixed,
    /// Данных пока не хватает (мало треков, мало участников, только вошли).
    /// Не ошибка: первые секунды встречи выглядят именно так.
    Unknown,
}

impl TrackMode {
    pub const ALL: [TrackMode; 4] = [
        TrackMode::PerParticipant,
        TrackMode::Slots,
        TrackMode::Mixed,
        TrackMode::Unknown,
    ];

    /// Строковая форма — она же уезжает в `meetings.track_mode` и в манифест.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerParticipant => "per_participant",
            Self::Slots => "slots",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// Можно ли считать «трек = человек».
    ///
    /// Единственный вопрос, ради ответа на который весь модуль и существует.
    pub fn tracks_are_people(self) -> bool {
        matches!(self, Self::PerParticipant)
    }
}

/// Наблюдение за треками — вход [`detect_track_mode`].
///
/// Отдельная структура, а не четыре аргумента: у функции появляются
/// признаки (Ф8 добавит `mid`-историю), и позиционные булевы аргументы
/// перепутать местами — вопрос времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackObservation {
    /// Сколько удалённых АУДИОтреков сейчас живо.
    pub audio_tracks: usize,
    /// Сколько участников видит DOM-проба, включая бота.
    pub participants: usize,
    /// Оставались ли `mid` дорожек прежними между наблюдениями.
    ///
    /// Ключевой признак: у SFU со слотами `mid` фиксирован, а вот КТО в этом
    /// слоте — меняется. Само по себе постоянство `mid` ничего не доказывает,
    /// оно работает только вместе с соотношением треков и участников.
    pub mid_stable: bool,
    /// Пришло ли в одном треке несколько CSRC с ненулевым уровнем.
    ///
    /// **Прямой признак микса** и единственный по-настоящему надёжный:
    /// `getContributingSources()` перечисляет источники, которые сервер
    /// смешал в эту дорожку. Несколько — значит в одном треке говорят разные
    /// люди, и пер-трековая атрибуция невозможна по определению.
    pub multi_csrc: bool,
    /// Сколько времени прошло с входа. До `warmup_ms` режим не определяется:
    /// в первые секунды треки ещё договариваются, и любой вывод был бы
    /// гаданием.
    pub since_join_ms: u64,
}

/// Сколько ждать после входа, прежде чем судить о режиме, мс.
///
/// Двадцать секунд из плана. Раньше этого момента SFU ещё досылает дорожки, и
/// «один трек при трёх участниках» встречается у площадки, которая через
/// секунду отдаст три.
pub const DEFAULT_MODE_WARMUP_MS: u64 = 20_000;

/// Сколько дорожек считается «фиксированным набором слотов».
///
/// Практика площадок: SFU форвардит 3–4 активных говорящих. Верхняя граница
/// нужна, чтобы «пять треков при шести участниках» не превратилось в слоты.
pub const SLOT_TRACKS_MAX: usize = 4;

/// Определяет, можно ли верить трекам как представителям людей.
///
/// **Чистая функция.** Ни ввода-вывода, ни времени, ни глобального состояния —
/// таблица в тестах перечисляет сочетания признаков построчно, и это
/// единственный способ не оставить дыру в логике, которую потом ловят на живой
/// встрече.
///
/// Порядок проверок существенен:
///
/// 1. **не прогрелись** → [`TrackMode::Unknown`];
/// 2. **несколько CSRC** → [`TrackMode::Mixed`] независимо от числа дорожек:
///    это прямое доказательство микса, а не косвенный признак;
/// 3. **треков нет** → `Unknown` (не «слоты»: судить не о чем);
/// 4. **один трек при >2 участниках** → `Mixed`;
/// 5. **дорожек мало и они не растут за участниками** → `Slots`;
/// 6. иначе → `PerParticipant`.
pub fn detect_track_mode(obs: &TrackObservation, warmup_ms: u64) -> TrackMode {
    if obs.since_join_ms < warmup_ms {
        return TrackMode::Unknown;
    }
    if obs.multi_csrc {
        return TrackMode::Mixed;
    }
    if obs.audio_tracks == 0 {
        return TrackMode::Unknown;
    }
    // Участников без бота: сам бот аудиотрека НЕ ПРИСЫЛАЕТ, поэтому сравнивать
    // число дорожек надо с числом собеседников, а не со списком целиком.
    let others = obs.participants.saturating_sub(1);
    if obs.audio_tracks == 1 && others > 1 {
        return TrackMode::Mixed;
    }
    // Слоты: дорожек фиксированно мало, а собеседников заметно больше. Именно
    // «заметно»: равенство 3 и 3 — это ещё трек на человека.
    if obs.audio_tracks <= SLOT_TRACKS_MAX && others > obs.audio_tracks && obs.mid_stable {
        return TrackMode::Slots;
    }
    if others > obs.audio_tracks + 1 {
        // Дорожек существенно меньше людей, и `mid` при этом плавает: набор
        // всё равно не персональный, верить ему нельзя.
        return TrackMode::Slots;
    }
    TrackMode::PerParticipant
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сокращение для табличного теста: (треки, участники, mid_stable,
    /// multi_csrc) → ожидаемый режим, всё после прогрева.
    fn mode(tracks: usize, participants: usize, mid_stable: bool, multi_csrc: bool) -> TrackMode {
        detect_track_mode(
            &TrackObservation {
                audio_tracks: tracks,
                participants,
                mid_stable,
                multi_csrc,
                since_join_ms: DEFAULT_MODE_WARMUP_MS,
            },
            DEFAULT_MODE_WARMUP_MS,
        )
    }

    /// ТАБЛИЦА: строка на каждое осмысленное сочетание признаков.
    #[test]
    fn the_mode_table_holds() {
        #[rustfmt::skip]
        let cases: &[(usize, usize, bool, bool, TrackMode, &str)] = &[
            // треки, участники, mid, csrc, ожидание, почему
            (3, 4, true,  false, TrackMode::PerParticipant, "трое собеседников — три дорожки"),
            (2, 3, false, false, TrackMode::PerParticipant, "двое собеседников — две дорожки, mid плавает при renegotiation"),
            (1, 2, true,  false, TrackMode::PerParticipant, "один на один"),
            (4, 9,  true,  false, TrackMode::Slots,          "восемь собеседников на четырёх дорожках"),
            (3, 8,  true,  false, TrackMode::Slots,          "классический SFU со слотами"),
            (3, 8,  false, false, TrackMode::Slots,          "дорожек вдвое меньше людей — верить нечему даже при плавающем mid"),
            (1, 5,  true,  false, TrackMode::Mixed,          "одна дорожка на четверых собеседников"),
            (1, 2,  true,  true,  TrackMode::Mixed,          "несколько CSRC — прямое доказательство микса"),
            (4, 5,  true,  true,  TrackMode::Mixed,          "CSRC перевешивает соотношение дорожек"),
            (0, 3,  true,  false, TrackMode::Unknown,        "дорожек нет — судить не о чем"),
            (2, 0,  true,  false, TrackMode::PerParticipant, "DOM-проба протухла: участников не видно, треки есть"),
            // vcc.airlabs.pro, ЗАМЕР Ф13: бот + двое собеседников, три
            // удалённых аудиодорожки (mid=0, mid=1 и ГЛУХОЙ СЛОТ mid=2 без
            // потока и с `muted = true`), `getContributingSources()` пуст на
            // всех трёх. Лишняя дорожка тянет наблюдение В СТОРОНУ слотов —
            // и именно поэтому кейс стоит здесь: `SLOT_TRACKS_MAX = 4`, и
            // строка доказывает, что запас про слоты не съедает площадку,
            // где трек РЕАЛЬНО равен человеку.
            (3, 3,  true,  false, TrackMode::PerParticipant, "vcc: двое собеседников, две живые дорожки плюс глухой слот"),
            (2, 3,  true,  false, TrackMode::PerParticipant, "vcc без глухого слота — ответ обязан не измениться"),
            (2, 2,  true,  false, TrackMode::PerParticipant, "vcc один на один плюс глухой слот"),
            (4, 4,  true,  false, TrackMode::PerParticipant, "vcc втроём: три дорожки плюс глухой слот, всё ещё трек = человек"),
        ];
        for &(tracks, participants, mid, csrc, want, why) in cases {
            assert_eq!(
                mode(tracks, participants, mid, csrc),
                want,
                "{tracks} треков / {participants} участников / mid={mid} / csrc={csrc}: {why}"
            );
        }
    }

    /// ГЛУХОЙ СЛОТ НЕ МЕНЯЕТ ОТВЕТА.
    ///
    /// Живой замер на vcc.airlabs.pro: LiveKit держит про запас лишний
    /// удалённый аудиотрансивер — дорожка есть, потока у неё нет
    /// (`streams: []`), `track.muted` остаётся `true` всю встречу, к
    /// `<audio>` она не привязана. Считать её участником нельзя, но и
    /// вычитать из наблюдения тоже нечем: с точки зрения `getReceivers()` она
    /// такая же дорожка, как остальные.
    ///
    /// Поэтому детект обязан выдержать ЛИШНЮЮ дорожку в любую сторону — и
    /// именно это проверяется парой «с слотом / без слота» на одинаковом
    /// числе людей.
    #[test]
    fn a_deaf_spare_slot_never_turns_per_participant_into_slots() {
        for people in 2..=6usize {
            let live = people - 1;
            let without = mode(live, people, true, false);
            let with_spare = mode(live + 1, people, true, false);
            assert_eq!(
                without,
                TrackMode::PerParticipant,
                "{people} участников, {live} дорожек"
            );
            assert_eq!(
                with_spare, without,
                "{people} участников: глухой слот изменил вердикт"
            );
        }
    }

    /// До прогрева ответ всегда `Unknown`, каким бы ни был набор признаков: в
    /// первые секунды SFU ещё досылает дорожки.
    #[test]
    fn before_the_warmup_nothing_is_decided() {
        for since in [0u64, 1, DEFAULT_MODE_WARMUP_MS - 1] {
            let obs = TrackObservation {
                audio_tracks: 1,
                participants: 9,
                mid_stable: true,
                multi_csrc: true,
                since_join_ms: since,
            };
            assert_eq!(
                detect_track_mode(&obs, DEFAULT_MODE_WARMUP_MS),
                TrackMode::Unknown,
                "since_join_ms = {since}"
            );
        }
    }

    /// Тесты обязаны уметь прогреть детект мгновенно — иначе каждый прогон
    /// стоил бы двадцати секунд.
    #[test]
    fn the_warmup_is_a_parameter_not_a_constant() {
        let obs = TrackObservation {
            audio_tracks: 3,
            participants: 4,
            mid_stable: true,
            multi_csrc: false,
            since_join_ms: 1,
        };
        assert_eq!(detect_track_mode(&obs, 0), TrackMode::PerParticipant);
        assert_eq!(detect_track_mode(&obs, 20_000), TrackMode::Unknown);
    }

    /// Функция обязана быть чистой: тот же вход — тот же выход, сколько ни
    /// зови.
    #[test]
    fn the_function_is_pure() {
        let obs = TrackObservation {
            audio_tracks: 3,
            participants: 8,
            mid_stable: true,
            multi_csrc: false,
            since_join_ms: 60_000,
        };
        let first = detect_track_mode(&obs, DEFAULT_MODE_WARMUP_MS);
        for _ in 0..100 {
            assert_eq!(detect_track_mode(&obs, DEFAULT_MODE_WARMUP_MS), first);
        }
        assert_eq!(first, TrackMode::Slots);
    }

    #[test]
    fn only_per_participant_lets_us_trust_tracks() {
        assert!(TrackMode::PerParticipant.tracks_are_people());
        for m in [TrackMode::Slots, TrackMode::Mixed, TrackMode::Unknown] {
            assert!(!m.tracks_are_people(), "{}", m.as_str());
        }
    }

    /// У режима, как и у статуса, обязана быть ОДНА строковая форма: `as_str`
    /// уходит в колонку `meetings.track_mode`, serde — в манифест и WS-кадры.
    #[test]
    fn the_serde_form_equals_the_string_form() {
        for m in TrackMode::ALL {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(json, format!("\"{}\"", m.as_str()), "{m:?}");
            assert_eq!(serde_json::from_str::<TrackMode>(&json).unwrap(), m);
            assert_eq!(TrackMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(TrackMode::parse("per-participant"), None);
    }
}
