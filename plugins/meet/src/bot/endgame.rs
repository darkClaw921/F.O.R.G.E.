//! Детект конца встречи и автовыход. **Чистая логика, без браузера.**
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Сигналов НЕСКОЛЬКО, и они НЕЗАВИСИМЫ
//!
//! Это не перестраховка, а прямое следствие инварианта фазы: DOM-проба может
//! протухнуть в любой день, и если бы конец встречи ловился только ею, бот
//! оставался бы в пустой комнате навсегда — писал бы тишину, держал слот
//! записи и жёг батарею.
//!
//! | сигнал | откуда | протухает вместе с вёрсткой? |
//! |---|---|---|
//! | [`StopReason::BrowserGone`] | target CDP упал | нет |
//! | [`StopReason::AllTracksEnded`] | все дорожки в состоянии `ended` | нет |
//! | [`StopReason::DomEnded`] | DOM-маркер или пост-звонковый адрес | ДА |
//! | [`StopReason::MaxDuration`] | потолок длительности | нет |
//! | [`StopReason::Alone`] | бот остался один | да, косвенно |
//! | [`StopReason::Silence`] | тишина при ≤1 собеседнике | да, косвенно |
//! | [`StopReason::UserStop`] / [`StopReason::UserCancel`] | человек | нет |
//!
//! Отключите DOM-пробу целиком — останутся четыре сигнала, и на это есть тест.
//!
//! # Почему автовыходы требуют, чтобы пробы ХОТЬ РАЗ сработали
//!
//! «Участников нет» и «пробы промахнулись» выглядят из кода одинаково —
//! пустым списком. Считай мы промах одиночеством, протухший селектор
//! ВЫКИДЫВАЛ БЫ БОТА со встречи через минуту после входа, причём тем увереннее,
//! чем свежее вёрстка площадки. Поэтому [`EndSignals::participants_known`]
//! обязателен: пока список участников не удался ни разу, ни `alone`, ни
//! `silence` не срабатывают вовсе.
//!
//! # Все таймауты — поля, а не константы
//!
//! Иначе тест потолка длительности шёл бы четыре часа, а тест одиночества —
//! минуту. Здесь они принимаются миллисекундами, и вся таблица проверок
//! отрабатывает за доли секунды.

use crate::config::MeetConfig;
use crate::hub::event::StopReason;

/// Таймауты автовыхода в миллисекундах. `0` — сигнал выключен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndConfig {
    /// Бот остался один.
    pub alone_ms: u64,
    /// Тишина при не более чем одном собеседнике.
    pub silence_ms: u64,
    /// Потолок длительности встречи.
    pub max_ms: u64,
}

impl Default for EndConfig {
    fn default() -> Self {
        Self::from(&MeetConfig::default())
    }
}

impl From<&MeetConfig> for EndConfig {
    fn from(cfg: &MeetConfig) -> Self {
        Self {
            alone_ms: cfg.alone_timeout_secs.saturating_mul(1_000),
            silence_ms: cfg.silence_timeout_secs.saturating_mul(1_000),
            max_ms: cfg.max_meeting_secs.saturating_mul(1_000),
        }
    }
}

/// Что бот видит в момент наблюдения.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndSignals {
    /// Target CDP упал: вкладку закрыли, Chrome умер.
    pub target_gone: bool,
    /// Сколько дорожек появлялось за встречу.
    pub tracks_seen: usize,
    /// Сколько дорожек живо прямо сейчас.
    pub tracks_live: usize,
    /// Сработал DOM-маркер конца встречи.
    pub dom_ended: bool,
    /// Адрес ушёл на пост-звонковую страницу.
    pub post_call: bool,
    /// Пробы участников срабатывали хоть раз за встречу.
    pub participants_known: bool,
    /// Участников кроме бота прямо сейчас.
    pub others: usize,
    /// Кто-то говорит прямо сейчас (VAD дорожек либо DOM-индикатор).
    pub speaking: bool,
}

/// Наблюдатель за концом встречи.
#[derive(Debug, Clone)]
pub struct EndWatch {
    cfg: EndConfig,
    alone_since: Option<u64>,
    silent_since: Option<u64>,
}

impl EndWatch {
    pub fn new(cfg: EndConfig) -> Self {
        Self {
            cfg,
            alone_since: None,
            silent_since: None,
        }
    }

    pub fn config(&self) -> EndConfig {
        self.cfg
    }

    /// Принимает наблюдение и отвечает, пора ли выходить.
    ///
    /// `elapsed_ms` — время от старта бота, а не от `T0` записи: потолок
    /// длительности обязан работать и тогда, когда запись так и не началась
    /// (вошли, а звука нет), иначе зависший на входе бот жил бы вечно.
    pub fn observe(&mut self, elapsed_ms: u64, s: EndSignals) -> Option<StopReason> {
        // 1. Браузера нет — обсуждать нечего.
        if s.target_gone {
            return Some(StopReason::BrowserGone);
        }

        // 2. Все дорожки кончились. Требование «была хоть одна» обязательно:
        //    до первой дорожки живых тоже ноль, и без него бот выходил бы
        //    ровно в момент входа.
        if s.tracks_seen > 0 && s.tracks_live == 0 {
            return Some(StopReason::AllTracksEnded);
        }

        // 3. Единственный сигнал, зависящий от вёрстки.
        if s.dom_ended || s.post_call {
            return Some(StopReason::DomEnded);
        }

        // 4. Потолок.
        if self.cfg.max_ms > 0 && elapsed_ms >= self.cfg.max_ms {
            return Some(StopReason::MaxDuration);
        }

        // 5-6. Автовыходы по бездействию. Оба слепы, пока пробы не сработали
        //      ни разу: промах селектора не имеет права выглядеть пустой
        //      комнатой.
        if !s.participants_known {
            self.alone_since = None;
            self.silent_since = None;
            return None;
        }

        if s.others == 0 {
            let since = *self.alone_since.get_or_insert(elapsed_ms);
            if self.cfg.alone_ms > 0 && elapsed_ms.saturating_sub(since) >= self.cfg.alone_ms {
                return Some(StopReason::Alone);
            }
        } else {
            self.alone_since = None;
        }

        if s.others <= 1 && !s.speaking {
            let since = *self.silent_since.get_or_insert(elapsed_ms);
            if self.cfg.silence_ms > 0 && elapsed_ms.saturating_sub(since) >= self.cfg.silence_ms {
                return Some(StopReason::Silence);
            }
        } else {
            self.silent_since = None;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Миллисекундные таймауты: тест потолка иначе шёл бы четыре часа.
    fn fast() -> EndConfig {
        EndConfig {
            alone_ms: 100,
            silence_ms: 200,
            max_ms: 1_000,
        }
    }

    /// Живая встреча: двое говорят, ничего не срабатывает.
    fn alive() -> EndSignals {
        EndSignals {
            tracks_seen: 2,
            tracks_live: 2,
            participants_known: true,
            others: 2,
            speaking: true,
            ..EndSignals::default()
        }
    }

    /// ТАБЛИЧНАЯ ПРОВЕРКА: каждый сигнал по отдельности доводит до выхода.
    #[test]
    fn every_signal_on_its_own_ends_the_meeting() {
        let cases: Vec<(&str, EndSignals, u64, StopReason)> = vec![
            (
                "target упал",
                EndSignals { target_gone: true, ..alive() },
                10,
                StopReason::BrowserGone,
            ),
            (
                "все дорожки кончились",
                EndSignals { tracks_live: 0, ..alive() },
                10,
                StopReason::AllTracksEnded,
            ),
            (
                "DOM-маркер конца",
                EndSignals { dom_ended: true, ..alive() },
                10,
                StopReason::DomEnded,
            ),
            (
                "пост-звонковый адрес",
                EndSignals { post_call: true, ..alive() },
                10,
                StopReason::DomEnded,
            ),
            ("потолок длительности", alive(), 1_000, StopReason::MaxDuration),
        ];
        for (what, signals, elapsed, want) in cases {
            let mut watch = EndWatch::new(fast());
            assert_eq!(watch.observe(elapsed, signals), Some(want), "{what}");
        }
    }

    /// Живая встреча не кончается сама.
    #[test]
    fn a_live_meeting_is_not_ended_by_anything() {
        let mut watch = EndWatch::new(fast());
        for t in [0, 100, 500, 999] {
            assert_eq!(watch.observe(t, alive()), None, "на {t} мс бот собрался выходить");
        }
    }

    /// «Бот остался один» ждёт СВОЙ таймаут, а не срабатывает сразу.
    #[test]
    fn being_alone_waits_for_its_timeout() {
        let mut watch = EndWatch::new(fast());
        let alone = EndSignals {
            others: 0,
            speaking: false,
            ..alive()
        };
        assert_eq!(watch.observe(0, alone), None);
        assert_eq!(watch.observe(99, alone), None, "выход раньше таймаута");
        assert_eq!(watch.observe(100, alone), Some(StopReason::Alone));
    }

    /// Вернувшийся участник СБРАСЫВАЕТ отсчёт: перезагрузка вкладки не повод
    /// выкидывать бота.
    #[test]
    fn a_returning_participant_resets_the_countdown() {
        let mut watch = EndWatch::new(fast());
        let alone = EndSignals { others: 0, speaking: false, ..alive() };
        assert_eq!(watch.observe(0, alone), None);
        assert_eq!(watch.observe(50, alive()), None, "участник вернулся");
        assert_eq!(watch.observe(120, alone), None, "отсчёт обязан начаться заново");
        assert_eq!(
            watch.observe(150, alone),
            None,
            "с момента ухода прошло 30 мс из 100 — выходить рано"
        );
        assert_eq!(watch.observe(220, alone), Some(StopReason::Alone));
    }

    /// Тишина при одном собеседнике: речь сбрасывает отсчёт.
    #[test]
    fn silence_with_one_companion_ends_the_meeting_and_speech_resets_it() {
        let mut watch = EndWatch::new(fast());
        let quiet = EndSignals {
            others: 1,
            speaking: false,
            ..alive()
        };
        assert_eq!(watch.observe(0, quiet), None);
        assert_eq!(watch.observe(150, quiet), None);
        assert_eq!(
            watch.observe(160, EndSignals { speaking: true, ..quiet }),
            None,
            "кто-то заговорил"
        );
        assert_eq!(watch.observe(300, quiet), None, "отсчёт обязан начаться заново");
        assert_eq!(watch.observe(500, quiet), Some(StopReason::Silence));
    }

    /// Тишина при ДВОИХ собеседниках не считается: люди могут молчать, глядя
    /// в презентацию.
    #[test]
    fn silence_with_two_companions_is_not_a_reason_to_leave() {
        let mut watch = EndWatch::new(fast());
        let quiet = EndSignals { others: 2, speaking: false, ..alive() };
        for t in [0, 300, 900] {
            assert_eq!(watch.observe(t, quiet), None, "на {t} мс");
        }
    }

    /// ГЛАВНЫЙ СТРАЖ: протухшие пробы НЕ выкидывают бота со встречи.
    ///
    /// Пустой список участников означает и «никого нет», и «селектор больше не
    /// находится». Второе случается при каждой пересборке фронта площадки, и
    /// перепутай мы их — бот уходил бы через минуту после входа, унося с собой
    /// запись живой встречи.
    #[test]
    fn rotten_probes_never_kick_the_bot_out() {
        let mut watch = EndWatch::new(fast());
        let blind = EndSignals {
            participants_known: false,
            others: 0,
            speaking: false,
            ..alive()
        };
        for t in [0, 100, 500, 999] {
            assert_eq!(watch.observe(t, blind), None, "на {t} мс сработал слепой автовыход");
        }
        // А независимые сигналы при этом продолжают работать.
        assert_eq!(watch.observe(1_000, blind), Some(StopReason::MaxDuration));
    }

    /// Отключённая DOM-проба конца встречи не мешает завершиться по остальным.
    #[test]
    fn without_the_dom_probe_the_meeting_still_ends() {
        let mut watch = EndWatch::new(fast());
        // Пробы конца нет вовсе: `dom_ended` и `post_call` всегда false.
        let signals = EndSignals {
            dom_ended: false,
            post_call: false,
            tracks_live: 0,
            ..alive()
        };
        assert_eq!(watch.observe(10, signals), Some(StopReason::AllTracksEnded));

        let mut watch = EndWatch::new(fast());
        let signals = EndSignals {
            dom_ended: false,
            post_call: false,
            target_gone: true,
            ..alive()
        };
        assert_eq!(watch.observe(10, signals), Some(StopReason::BrowserGone));
    }

    /// До первой дорожки «все дорожки кончились» не срабатывает.
    #[test]
    fn no_tracks_yet_is_not_all_tracks_ended() {
        let mut watch = EndWatch::new(fast());
        let fresh = EndSignals {
            tracks_seen: 0,
            tracks_live: 0,
            participants_known: true,
            others: 2,
            speaking: false,
            ..EndSignals::default()
        };
        assert_eq!(watch.observe(0, fresh), None);
    }

    /// Ноль в таймауте выключает сигнал: так бот пишет встречу без потолка.
    #[test]
    fn a_zero_timeout_disables_the_signal() {
        let mut watch = EndWatch::new(EndConfig {
            alone_ms: 0,
            silence_ms: 0,
            max_ms: 0,
        });
        let alone = EndSignals { others: 0, speaking: false, ..alive() };
        assert_eq!(watch.observe(10_000_000, alone), None);
    }

    /// Секунды конфига переводятся в миллисекунды здесь и больше нигде.
    #[test]
    fn the_config_translates_seconds_into_milliseconds() {
        let cfg = EndConfig::from(&MeetConfig::default());
        assert_eq!(cfg.alone_ms, 60_000);
        assert_eq!(cfg.silence_ms, 900_000);
        assert_eq!(cfg.max_ms, 4 * 60 * 60 * 1_000);
    }
}
