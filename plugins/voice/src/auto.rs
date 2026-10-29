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
//!        ─▶ беседа скрыта ─▶ беседа вне скоупа ─▶ слишком длинное
//!        ─▶ РАЗБИРАЕМ
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
    /// Беседа вне белого списка АВТОРАЗБОРА (скоупа).
    ///
    /// Отдельная причина, а не [`Skip::Hidden`]: списки разные и вопросы
    /// разные («не показывай мне этот чат» против «не трать GPU на этот чат»),
    /// и человеку, ищущему в журнале, почему у него ничего не расшифровалось,
    /// нужно знать, КАКУЮ из двух форм открывать.
    OutOfScope,
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
            Self::OutOfScope => "out_of_scope",
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
    /// ГРУППЫ беседы в лексике настроек — то, чем оперируют оба белых списка.
    ///
    /// У VK и Bitrix группа ровно одна (вид беседы: `user`/`chat`/`community`,
    /// `user`/`chat`/`task`), у Telegram это ключи ПАПОК аккаунта, и их у чата
    /// несколько (`inbox` плюс каждая пользовательская папка, которой чат
    /// подходит). Пустой срез — «групп не знаем»: при действующем белом списке
    /// такая беседа не проходит ни видимость, ни скоуп, и решает только
    /// поимённый выбор.
    pub peer_groups: &'a [&'a str],
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
    if !prefs.peer_visible(cand.peer, cand.peer_groups) {
        return Err(Skip::Hidden);
    }
    // Скоуп — ПОСЛЕ видимости и ДО длительности. После видимости, потому что
    // скрытая беседа не должна проявляться в журнале даже причиной отказа; до
    // длительности, потому что длительность у части площадок известна лишь
    // после декодирования звука, а значит стоит дороже сравнения строк.
    if !prefs.peer_in_scope(cand.peer, cand.peer_groups) {
        return Err(Skip::OutOfScope);
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
            peer_groups: &["chat"],
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
            ..Prefs::default()
        };
        assert_eq!(decide(&prefs, &voice(), 900), Err(Skip::Hidden));

        // А отмеченная поимённо — разбирается.
        let cand = Candidate {
            peer: "777",
            peer_groups: &["user"],
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

    /// **ПУСТОЙ СКОУП РАЗБИРАЕТ ВЕЗДЕ — ПОВЕДЕНИЕ ДО ФИЧИ НЕ ИЗМЕНИЛОСЬ.**
    ///
    /// Это главное обещание фазы: у человека, который новую форму ни разу не
    /// открывал, лестница обязана давать РОВНО те же ответы, что и до неё.
    #[test]
    fn an_empty_scope_changes_nothing() {
        for (peer, peer_groups) in [
            ("2000000042", &["chat"][..]),
            ("777", &["user"][..]),
            ("chat7", &["task"][..]),
            ("-99", &[][..]),
        ] {
            let cand = Candidate {
                peer,
                peer_groups,
                ..voice()
            };
            assert_eq!(decide(&on(), &cand, 900), Ok(()), "{peer:?}/{peer_groups:?}");
        }
        // И прежние причины отказа остались прежними.
        assert_eq!(decide(&Prefs::default(), &voice(), 900), Err(Skip::Disabled));
        let out = Candidate {
            outgoing: true,
            ..voice()
        };
        assert_eq!(decide(&on(), &out, 900), Err(Skip::Outgoing));
        let long = Candidate {
            duration_secs: Some(1200),
            ..voice()
        };
        assert_eq!(
            decide(&on(), &long, 900),
            Err(Skip::TooLong {
                secs: 1200,
                limit: 900
            })
        );
    }

    /// **НЕПУСТОЙ СКОУП — БЕЛЫЙ СПИСОК: ВЫБРАННОЕ РАЗБИРАЕТСЯ, ОСТАЛЬНОЕ НЕТ.**
    #[test]
    fn a_peer_outside_the_scope_is_skipped_by_its_own_reason() {
        let prefs = Prefs {
            autotranscribe: true,
            scope_peers: vec!["777".into()],
            ..Prefs::default()
        };
        // Выбранный поимённо — разбирается.
        let chosen = Candidate {
            peer: "777",
            peer_groups: &["user"],
            ..voice()
        };
        assert_eq!(decide(&prefs, &chosen, 900), Ok(()));
        // Любой другой — нет, и причина называется своим словом.
        let skipped = decide(&prefs, &voice(), 900).unwrap_err();
        assert_eq!(skipped, Skip::OutOfScope);
        assert_eq!(skipped.code(), "out_of_scope");
        assert_ne!(skipped.code(), Skip::Hidden.code(), "не путать с видимостью");
    }

    /// **ВЫБОР ГРУППЫ ДЕЙСТВУЕТ НА ВСЕ БЕСЕДЫ ЭТОЙ ГРУППЫ.**
    ///
    /// Папка Telegram, тип беседы VK, вид диалога Bitrix — одна и та же
    /// ступень: словарь групп плагин приводит сам.
    #[test]
    fn choosing_a_group_covers_every_peer_in_it() {
        let prefs = Prefs {
            autotranscribe: true,
            scope_kinds: vec!["chat".into()],
            ..Prefs::default()
        };
        for peer in ["2000000042", "2000000099", "chat7"] {
            let cand = Candidate {
                peer,
                peer_groups: &["chat"],
                ..voice()
            };
            assert_eq!(decide(&prefs, &cand, 900), Ok(()), "{peer}");
        }
        let other = Candidate {
            peer: "42",
            peer_groups: &["user"],
            ..voice()
        };
        assert_eq!(decide(&prefs, &other, 900), Err(Skip::OutOfScope));
        // Группа неизвестна (Telegram не выводит папку из id) — беседа вне
        // скоупа, пока не выбрана поимённо.
        let unknown = Candidate {
            peer: "42",
            peer_groups: &[],
            ..voice()
        };
        assert_eq!(decide(&prefs, &unknown, 900), Err(Skip::OutOfScope));
    }

    /// **ВИДИМОСТЬ — ВЫШЕСТОЯЩИЙ ФИЛЬТР: СКРЫТАЯ БЕСЕДА НЕ РАЗБИРАЕТСЯ ДАЖЕ
    /// ПОПАВ В СКОУП.**
    ///
    /// И причина в журнале именно `hidden`: про скрытую беседу мы не
    /// рассказываем ничего сверх того, что она скрыта.
    #[test]
    fn visibility_outranks_the_scope() {
        let prefs = Prefs {
            autotranscribe: true,
            filter_enabled: true,
            visible_peers: vec!["777".into()],
            scope_peers: vec!["2000000042".into()],
            ..Prefs::default()
        };
        assert_eq!(decide(&prefs, &voice(), 900), Err(Skip::Hidden));
    }

    /// **ДЛИННОЕ ГОЛОСОВОЕ ВНЕ СКОУПА ДАЁТ `OutOfScope`, А НЕ `TooLong`.**
    ///
    /// Доказательство порядка ступеней: скоуп стоит ДО длительности. Обратный
    /// порядок означал бы, что про беседу, которую человек не выбирал, мы
    /// сначала выясняем длительность её записи.
    #[test]
    fn the_scope_is_checked_before_the_duration() {
        let prefs = Prefs {
            autotranscribe: true,
            scope_peers: vec!["777".into()],
            ..Prefs::default()
        };
        let long_and_foreign = Candidate {
            duration_secs: Some(1200),
            ..voice()
        };
        assert_eq!(decide(&prefs, &long_and_foreign, 900), Err(Skip::OutOfScope));
        // А выбранная беседа с той же записью упирается уже в потолок.
        let long_and_chosen = Candidate {
            peer: "777",
            peer_groups: &["user"],
            duration_secs: Some(1200),
            ..voice()
        };
        assert_eq!(
            decide(&prefs, &long_and_chosen, 900),
            Err(Skip::TooLong {
                secs: 1200,
                limit: 900
            })
        );
    }

    /// **ВЫКЛЮЧЕННЫЙ ТУМБЛЕР ОТСЕКАЕТ РАНЬШЕ СКОУПА.**
    ///
    /// Скоуп — это выбор, а не второй тумблер: при выключенном авторазборе
    /// причина остаётся `disabled`, что бы в скоупе ни лежало.
    #[test]
    fn the_switch_still_wins_over_the_scope() {
        let prefs = Prefs {
            scope_peers: vec!["2000000042".into()],
            ..Prefs::default()
        };
        assert_eq!(decide(&prefs, &voice(), 900), Err(Skip::Disabled));
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
