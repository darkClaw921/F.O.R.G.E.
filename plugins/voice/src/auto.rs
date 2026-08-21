//! Решение «разбирать ли это входящее голосовое само» — общее на три плагина.
//!
//! Воркеры авторазбора у VK, Telegram и Bitrix разные (у каждого свой поток
//! событий), а ВОПРОС у них один и тот же, и ответы обязаны совпадать. Поэтому
//! здесь живут две вещи: чистая функция решения [`decide`] и маленький реестр
//! «это мы уже брали» [`SeenKeys`].
//!
//! # ПОРЯДОК СТУПЕНЕЙ — ЭТО ГЕЙТ ФАЗЫ, А НЕ ВКУСОВЩИНА
//!
//! ```text
//! тумблер выключен ─▶ своё исходящее ─▶ не голосовое
//!        ─▶ беседа скрыта ─▶ слишком длинное ─▶ РАЗБИРАЕМ
//! ```
//!
//! Тумблер проверяется ПЕРВЫМ и стоит один разбор булева поля. Ни докачки, ни
//! запуска движка, ни даже похода в стор при выключенном тумблере не
//! происходит — это обещание фазы, и оно держится тем, что все дорогие
//! действия физически стоят ПОСЛЕ этой функции.
//!
//! # ЧТО ЗДЕСЬ НЕ ПРОВЕРЯЕТСЯ
//!
//! Наличие ffmpeg, весов и ключа — это [`crate::gate::check`], и зовётся она
//! позже, уже в воркере: настройки машины меняются реже, чем приходят
//! сообщения, но проверять их до тумблера значило бы читать диск на КАЖДОЕ
//! входящее сообщение, включая текстовые.
//!
//! Потолок параллелизма тоже не здесь: он живёт в [`crate::hub::VoiceHub`] и
//! берётся ПОД ЗАМКОМ вместе со слотом. Авторазбор ходит через тот же реестр,
//! что и кнопка «расшифровать», — свой пул означал бы, что человек, нажавший
//! кнопку, стоит в очереди за фоновой работой.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::gate;
use crate::prefs::Prefs;
use crate::store::VoiceKey;

/// Сколько ключей помнит [`SeenKeys`] по умолчанию.
///
/// Пятьсот двенадцать — заведомо больше, чем принесёт один догон истории после
/// переподключения, и заведомо меньше, чем заметно по памяти (ключ — три
/// коротких строки).
pub const DEFAULT_SEEN_CAPACITY: usize = 512;

/// Почему голосовое НЕ разбирается само.
///
/// Не `bool`, потому что каждая причина попадает в журнал: «почему у меня
/// ничего не расшифровалось» — первый вопрос, который задают о фоновой фиче, и
/// ответ на него обязан быть в логах, а не в голове разработчика.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// Тумблер выключен — самая частая и самая дешёвая причина.
    Disabled,
    /// Своё исходящее: человек знает, что он сказал.
    Outgoing,
    /// Вложение не голосовое (или его нет вовсе).
    NotVoice(String),
    /// Беседа вне белого списка видимости.
    Hidden,
    /// Запись длиннее потолка `max_duration_secs`.
    TooLong {
        /// Длительность записи, секунды.
        secs: i64,
        /// Потолок из настроек, секунды.
        limit: u64,
    },
}

impl Skip {
    /// Короткое машинное имя причины — для журнала и тестов.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Outgoing => "outgoing",
            Self::NotVoice(_) => "not_voice",
            Self::Hidden => "hidden",
            Self::TooLong { .. } => "too_long",
        }
    }
}

/// Что плагин знает о новом сообщении к моменту решения.
#[derive(Debug, Clone, Default)]
pub struct Candidate<'a> {
    /// Вид вложения в лексике плагина (`voice`, `audio`, `photo`, …).
    pub kind: &'a str,
    /// Исходящее ли сообщение (`out` в DTO всех трёх плагинов).
    pub outgoing: bool,
    /// Длительность из метаданных мессенджера; `None` — он её не прислал
    /// (Bitrix не присылает никогда).
    pub duration_secs: Option<i64>,
    /// Идентификатор беседы в лексике настроек видимости.
    pub peer: &'a str,
    /// Вид беседы в лексике настроек видимости (`user`/`chat`/`community` у
    /// VK, `user`/`chat`/`task` у Bitrix); `None` — плагин его не выводит
    /// (Telegram: принадлежность чата ПАПКЕ из одного id не следует).
    pub peer_kind: Option<&'a str>,
}

/// Разбирать ли это вложение автоматически.
///
/// # Errors
///
/// [`Skip`] с причиной. Ошибкой это названо ради `?` в воркере: «не берём» —
/// нормальный, самый частый исход, и он обязан быть коротким в коде.
pub fn decide(prefs: &Prefs, cand: &Candidate, max_duration_secs: u64) -> Result<(), Skip> {
    // ПЕРВОЙ — и ничего дорогого до неё.
    if !prefs.autotranscribe {
        return Err(Skip::Disabled);
    }
    if cand.outgoing {
        return Err(Skip::Outgoing);
    }
    if !gate::is_voice_kind(cand.kind) {
        return Err(Skip::NotVoice(cand.kind.trim().to_string()));
    }
    if !prefs.peer_visible(cand.peer, cand.peer_kind) {
        return Err(Skip::Hidden);
    }
    if let Some(secs) = cand.duration_secs {
        if max_duration_secs > 0 && secs > max_duration_secs as i64 {
            return Err(Skip::TooLong {
                secs,
                limit: max_duration_secs,
            });
        }
    }
    Ok(())
}

/// Ключи, которые авторазбор уже брал в работу.
///
/// # ЗАЧЕМ, ЕСЛИ ЕСТЬ ДЕДУП В РЕЕСТРЕ
///
/// Реестр ([`crate::hub::VoiceHub`]) не пускает ВТОРОЙ прогон, пока идёт
/// первый, а стор не даёт пересчитывать готовое. Между ними остаётся дыра:
/// прогон, который УЖЕ ЗАКОНЧИЛСЯ ОТКАЗОМ. Повторная доставка того же события
/// (переподключение лонгпола, догон истории) запускала бы его снова и снова —
/// то есть ровно то, что человек увидел бы как «оно молотит по кругу и тратит
/// деньги».
///
/// Поэтому авторазбор помнит ключи сам. Кнопка «расшифровать» этот реестр НЕ
/// смотрит: человек, нажавший её после отказа, просит повторить осознанно.
#[derive(Debug)]
pub struct SeenKeys {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    order: VecDeque<VoiceKey>,
    capacity: usize,
}

impl Default for SeenKeys {
    fn default() -> Self {
        Self::new(DEFAULT_SEEN_CAPACITY)
    }
}

impl SeenKeys {
    /// Реестр на `capacity` ключей (ноль трактуется как единица: реестр,
    /// который не помнит ничего, — это отключённый дедуп, а его здесь быть не
    /// должно).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                order: VecDeque::new(),
                capacity: capacity.max(1),
            }),
        }
    }

    /// Занимает ключ. `true` — взяли впервые, `false` — уже брали.
    ///
    /// Проверка и занятие ОДНОЙ операцией под замком: два события одного
    /// сообщения, пришедшие в одну миллисекунду из двух задач, иначе оба
    /// получили бы «впервые».
    pub fn claim(&self, key: &VoiceKey) -> bool {
        let mut g = self.guard();
        if g.order.iter().any(|k| k == key) {
            return false;
        }
        if g.order.len() >= g.capacity {
            g.order.pop_front();
        }
        g.order.push_back(key.clone());
        true
    }

    /// Забывает ключ — например, когда прогон отказал по ВНЕШНЕЙ причине
    /// (портал оборвал докачку), и следующая доставка события имеет право
    /// попробовать снова.
    pub fn forget(&self, key: &VoiceKey) {
        let mut g = self.guard();
        g.order.retain(|k| k != key);
    }

    /// Сколько ключей помним сейчас.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard().order.len()
    }

    /// Пуст ли реестр.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Отравленный замок — не повод ронять плагин: данные производные, худшее
    /// последствие — один лишний прогон.
    fn guard(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Prefs {
        Prefs {
            autotranscribe: true,
            ..Prefs::default()
        }
    }

    fn voice<'a>() -> Candidate<'a> {
        Candidate {
            kind: "voice",
            outgoing: false,
            duration_secs: Some(12),
            peer: "2000000042",
            peer_kind: Some("chat"),
        }
    }

    /// **ВЫКЛЮЧЕННЫЙ ТУМБЛЕР ОТСЕКАЕТ ВСЁ И ПЕРВЫМ.**
    ///
    /// Даже идеальное голосовое из видимой беседы. Это и есть обещание «ничего
    /// не происходит по умолчанию».
    #[test]
    fn the_switch_is_checked_first_and_stops_everything() {
        assert_eq!(decide(&Prefs::default(), &voice(), 900), Err(Skip::Disabled));
        // И причина именно тумблер, а не что-то ещё: включаем — проходит.
        assert_eq!(decide(&on(), &voice(), 900), Ok(()));
    }

    /// **СВОЁ ИСХОДЯЩЕЕ НЕ РАЗБИРАЕТСЯ.**
    #[test]
    fn my_own_message_is_never_transcribed() {
        let cand = Candidate {
            outgoing: true,
            ..voice()
        };
        assert_eq!(decide(&on(), &cand, 900), Err(Skip::Outgoing));
    }

    /// **НЕ ГОЛОСОВОЕ ПРОПУСКАЕТСЯ, И ВИД НАЗЫВАЕТСЯ ВСЛУХ.**
    ///
    /// Обычное аудио (пересланный трек, запись разговора) — разбирается: это
    /// то же решение, что у кнопки, и расходиться они не имеют права.
    #[test]
    fn a_non_voice_attachment_is_skipped_by_name() {
        for kind in ["photo", "video", "document", "sticker", ""] {
            let cand = Candidate { kind, ..voice() };
            assert_eq!(
                decide(&on(), &cand, 900),
                Err(Skip::NotVoice(kind.to_string())),
                "{kind:?}"
            );
        }
        for kind in ["voice", "audio", "voice_message", "audio_message"] {
            let cand = Candidate { kind, ..voice() };
            assert_eq!(decide(&on(), &cand, 900), Ok(()), "{kind:?}");
        }
    }

    /// **БЕСЕДА ВНЕ БЕЛОГО СПИСКА НЕ РАЗБИРАЕТСЯ ДАЖЕ ПРИ ВКЛЮЧЁННОМ
    /// ТУМБЛЕРЕ.**
    #[test]
    fn a_hidden_peer_is_skipped_even_when_the_switch_is_on() {
        let prefs = Prefs {
            autotranscribe: true,
            filter_enabled: true,
            visible_kinds: vec!["user".into()],
            visible_peers: vec!["777".into()],
        };
        assert_eq!(decide(&prefs, &voice(), 900), Err(Skip::Hidden));

        // А отмеченная поимённо — разбирается.
        let cand = Candidate {
            peer: "777",
            peer_kind: Some("user"),
            ..voice()
        };
        assert_eq!(decide(&prefs, &cand, 900), Ok(()));
    }

    /// **СЛИШКОМ ДЛИННАЯ ЗАПИСЬ ПРОПУСКАЕТСЯ С ЧИСЛАМИ В ПРИЧИНЕ.**
    ///
    /// И проверяется ПОСЛЕ видимости: про скрытую беседу мы вообще не должны
    /// рассказывать в журнале, даже длительностью её сообщений.
    #[test]
    fn a_too_long_recording_is_skipped_with_numbers() {
        let cand = Candidate {
            duration_secs: Some(1200),
            ..voice()
        };
        assert_eq!(
            decide(&on(), &cand, 900),
            Err(Skip::TooLong {
                secs: 1200,
                limit: 900
            })
        );
        // Потолок 0 — «без потолка» (так же читает его лестница).
        assert_eq!(decide(&on(), &cand, 0), Ok(()));
        // Длительности нет (Bitrix) — решает уже декодированный звук.
        let unknown = Candidate {
            duration_secs: None,
            ..voice()
        };
        assert_eq!(decide(&on(), &unknown, 900), Ok(()));
    }

    /// **ОДИН КЛЮЧ БЕРЁТСЯ РОВНО ОДИН РАЗ.**
    #[test]
    fn a_key_is_claimed_only_once() {
        let seen = SeenKeys::default();
        let key = VoiceKey::new("2000000042", "7001", 0);
        assert!(seen.claim(&key));
        assert!(!seen.claim(&key), "повторная доставка события");
        assert!(seen.claim(&VoiceKey::new("2000000042", "7002", 0)), "другое сообщение");
        assert_eq!(seen.len(), 2);

        // Забыли — можно снова.
        seen.forget(&key);
        assert!(seen.claim(&key));
    }

    /// **РЕЕСТР ОГРАНИЧЕН: САМЫЕ СТАРЫЕ КЛЮЧИ ВЫТЕСНЯЮТСЯ.**
    ///
    /// Иначе открытая на неделю панель копила бы ключи всех сообщений аккаунта.
    #[test]
    fn the_registry_is_bounded_and_evicts_the_oldest() {
        let seen = SeenKeys::new(3);
        for i in 0..3 {
            assert!(seen.claim(&VoiceKey::new("p", i.to_string(), 0)));
        }
        assert_eq!(seen.len(), 3);
        assert!(seen.claim(&VoiceKey::new("p", "3", 0)));
        assert_eq!(seen.len(), 3, "потолок держится");
        assert!(seen.claim(&VoiceKey::new("p", "0", 0)), "самый старый забыт");
        assert!(!seen.claim(&VoiceKey::new("p", "3", 0)), "свежий помнится");
    }
}
