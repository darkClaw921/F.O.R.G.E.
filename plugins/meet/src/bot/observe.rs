//! Разбор снимка страницы и ИМЕНОВАНИЕ участников.
//!
//! Модуль НЕ импортирует `axum` и `http`. Чистый: ни браузера, ни ввода-вывода
//! — поэтому вся логика деградации проверяется значениями, а не прогоном.
//!
//! # Три вещи, которые здесь живут
//!
//! | | зачем |
//! |---|---|
//! | [`PageSnapshot`] | форма того, что вернул `observe.js`; поля необязательны все до одного |
//! | [`ParticipantNamer`] | «Участник N» вместо пустого имени, номер закреплён за участником навсегда |
//! | [`Axis`] | перевод часов страницы в единственную ось времени записи |
//!
//! # Почему номера участников не пересчитываются
//!
//! Потому что номер уезжает в транскрипт. Пересчитай мы нумерацию на каждом
//! наблюдении — «Участник 2» в начале встречи и «Участник 2» в конце оказались
//! бы разными людьми, а исправить это после расшифровки нельзя ничем.
//! Поэтому номер выдаётся один раз на ключ и не переиспользуется даже после
//! ухода участника (то же правило, что у индексов дорожек в `capture.js`).
//!
//! # Ось времени: ОДНО измерение обоих часов
//!
//! Запись живёт на оси `performance.now()` страницы; `T0` ставится в момент
//! первого захваченного трека и приезжает в событии `recording_started`
//! ЕДИНСТВЕННЫМ доступным снаружи видом — `Date.now()` того же мгновения.
//! [`Axis::anchor`] один раз берёт оба часа, снятые в один момент
//! (`observe.js` возвращает `perf` и `unix` рядом), и вычисляет `perf` начала
//! оси. Дальше используется только монотонный `perf`: переведи пользователь
//! системные часы посреди встречи — таймлайн не дрогнет.

use std::collections::HashMap;

use serde::Deserialize;

use crate::hub::event::ParticipantInfo;

/// Участник глазами проб — ровно то, что вернул `observe.js`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RawParticipant {
    /// Стабильный ключ из атрибута плитки. `None` — площадка его не даёт
    /// (Телемост, измерено Ф0).
    #[serde(default)]
    pub key: Option<String>,
    /// Видимое имя. Пустое — законно: подписывает [`ParticipantNamer`].
    #[serde(default)]
    pub name: String,
    /// Индикатор «говорит сейчас».
    #[serde(default)]
    pub speaking: bool,
}

/// Что видно на экране входа и панели.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PageUi {
    #[serde(rename = "continueInBrowser", default)]
    pub continue_in_browser: bool,
    #[serde(rename = "nameInput", default)]
    pub name_input: bool,
    #[serde(rename = "joinButton", default)]
    pub join_button: bool,
    #[serde(rename = "leaveButton", default)]
    pub leave_button: bool,
    #[serde(default)]
    pub lobby: bool,
    #[serde(default)]
    pub captcha: bool,
    /// Кнопка «подключить звук компьютера» видна прямо сейчас.
    ///
    /// Есть, по сути, только у Zoom, и там она несущая: пока по ней не
    /// кликнули, в странице НЕТ НИ ОДНОГО источника звука — записывать нечего.
    #[serde(rename = "joinAudio", default)]
    pub join_audio: bool,
}

/// Состояние устройства бота.
///
/// Три значения, а не булево: «не знаю» — это НЕ «выключено». Ложная
/// уверенность здесь стоила бы бота, гудящего в чужую встречу, поэтому
/// [`DeviceState::Unknown`] заставляет адаптер жать кнопку повторно, а не
/// успокаиваться.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceState {
    On,
    Off,
    #[default]
    Unknown,
}

impl DeviceState {
    pub fn parse(s: &str) -> Self {
        match s {
            "on" => Self::On,
            "off" => Self::Off,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Unknown => "unknown",
        }
    }
}

/// Снимок страницы.
///
/// **Все поля необязательны.** Снимок приходит из чужого браузера через JSON,
/// и половина полей отсутствует в тот момент, когда они нужнее всего: страница
/// ещё грузится, пробы промахнулись, площадка сменила вёрстку. Обязательное
/// поле означало бы отказ разбора всего снимка — то есть потерю и тех данных,
/// которые пришли.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageSnapshot {
    /// `performance.now()` страницы в момент снимка.
    #[serde(default)]
    pub perf: f64,
    /// `Date.now()` того же мгновения.
    #[serde(default)]
    pub unix: f64,
    #[serde(default)]
    pub href: String,
    #[serde(default)]
    pub participants: Vec<RawParticipant>,
    /// Кого площадка считает говорящим.
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub speaking: bool,
    /// Сырое состояние микрофона: `on` | `off` | `unknown`. Читается через
    /// [`PageSnapshot::mic`] — перечисление, а не строка, потому что «не
    /// знаю» здесь третье полноправное значение.
    #[serde(rename = "mic", default)]
    pub mic_raw: String,
    #[serde(rename = "cam", default)]
    pub cam_raw: String,
    #[serde(default)]
    pub ui: PageUi,
    /// Площадка ОТКАЗАЛА во входе.
    ///
    /// Отдельный сигнал, а не разновидность «ничего знакомого на экране».
    /// Google Meet отклоняет заявку анонимного участника автоматически и за
    /// секунды, когда у хоста включён ограниченный доступ; разница между «нам
    /// отказали» и «мы ещё ждём» — это разница между честным объяснением
    /// человеку и минутой молчаливого ожидания таймаута.
    #[serde(default)]
    pub denied: bool,
    #[serde(default)]
    pub ended: bool,
    #[serde(rename = "postCall", default)]
    pub post_call: bool,
}

impl PageSnapshot {
    /// Разбирает JSON, вернувшийся из `Runtime.evaluate`.
    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    pub fn mic(&self) -> DeviceState {
        DeviceState::parse(&self.mic_raw)
    }

    pub fn cam(&self) -> DeviceState {
        DeviceState::parse(&self.cam_raw)
    }

    /// Сколько участников кроме бота.
    ///
    /// Имя бота сравнивается без учёта регистра и пробелов по краям: площадки
    /// нормализуют его по-своему.
    pub fn others(&self, bot_name: &str) -> usize {
        let bot = bot_name.trim().to_lowercase();
        self.participants
            .iter()
            .filter(|p| p.name.trim().to_lowercase() != bot)
            .count()
    }
}

/// Присваивает участникам устойчивые имена.
#[derive(Debug, Default)]
pub struct ParticipantNamer {
    /// Ключ участника → его номер. Номера не переиспользуются.
    numbers: HashMap<String, usize>,
    next: usize,
}

impl ParticipantNamer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Сколько участников встретилось за встречу.
    pub fn known(&self) -> usize {
        self.numbers.len()
    }

    /// Ключ участника: атрибут плитки, иначе имя, иначе позиция.
    ///
    /// Позиция — последний рубеж: без имени и без атрибута отличить двух
    /// безымянных участников не по чему, и притворяться, что можем, вреднее,
    /// чем честно привязаться к месту в списке.
    fn key_of(p: &RawParticipant, idx: usize) -> String {
        if let Some(k) = p.key.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            return format!("id:{k}");
        }
        let name = p.name.trim();
        if !name.is_empty() {
            return format!("name:{}", name.to_lowercase());
        }
        format!("pos:{idx}")
    }

    /// Превращает сырых участников в именованных.
    pub fn label(&mut self, raw: &[RawParticipant]) -> Vec<ParticipantInfo> {
        raw.iter()
            .enumerate()
            .map(|(idx, p)| {
                let key = Self::key_of(p, idx);
                let number = *self.numbers.entry(key.clone()).or_insert_with(|| {
                    self.next += 1;
                    self.next
                });
                let name = p.name.trim();
                ParticipantInfo {
                    platform_id: Some(key),
                    name: if name.is_empty() {
                        // ТА САМАЯ ДЕГРАДАЦИЯ. Она обязана быть именно такой:
                        // человек увидит «Участник 2», нажмёт play на его
                        // дорожке и подпишет сам.
                        format!("Участник {number}")
                    } else {
                        name.to_string()
                    },
                    speaking: p.speaking,
                }
            })
            .collect()
    }
}

/// Единственная ось времени записи.
#[derive(Debug, Clone, Copy, Default)]
pub struct Axis {
    /// `performance.now()` начала оси. `None` — записи ещё нет.
    anchor_perf: Option<f64>,
}

impl Axis {
    pub fn new() -> Self {
        Self::default()
    }

    /// Есть ли уже ось.
    pub fn is_set(self) -> bool {
        self.anchor_perf.is_some()
    }

    /// Привязывает ось по снимку и `T0` из события `recording_started`.
    ///
    /// Зовётся один раз; повторный вызов игнорируется — иначе ось поехала бы
    /// на каждом наблюдении вслед за подстройкой системных часов.
    pub fn anchor(&mut self, snap: &PageSnapshot, t0_unix_ms: i64) {
        if self.anchor_perf.is_some() || snap.unix <= 0.0 || t0_unix_ms <= 0 {
            return;
        }
        let elapsed = snap.unix - t0_unix_ms as f64;
        // Часы страницы отстают от `T0`? Значит снимок сделан раньше начала
        // записи — привязываться по нему нельзя.
        if elapsed < 0.0 {
            return;
        }
        self.anchor_perf = Some(snap.perf - elapsed);
    }

    /// Момент снимка на общей оси. `None` — оси ещё нет.
    pub fn at_ms(self, perf: f64) -> Option<u64> {
        let anchor = self.anchor_perf?;
        Some((perf - anchor).max(0.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(key: Option<&str>, name: &str, speaking: bool) -> RawParticipant {
        RawParticipant {
            key: key.map(str::to_string),
            name: name.to_string(),
            speaking,
        }
    }

    /// Снимок мок-встречи разбирается целиком.
    #[test]
    fn a_snapshot_from_the_page_parses() {
        let raw = r#"{"perf":1234.5,"unix":1700000001000,"href":"https://х/j/1",
            "participants":[{"key":"p0","name":"F.O.R.G.E.","speaking":false},
                            {"key":"p1","name":"Алиса","speaking":true}],
            "active":"Алиса","speaking":true,"mic":"on","cam":"off",
            "ui":{"continueInBrowser":false,"nameInput":false,"joinButton":false,
                  "leaveButton":true,"lobby":false,"captcha":false},
            "ended":false,"postCall":false}"#;
        let snap = PageSnapshot::parse(raw).unwrap();
        assert_eq!(snap.participants.len(), 2);
        assert_eq!(snap.active.as_deref(), Some("Алиса"));
        assert_eq!(snap.mic(), DeviceState::On);
        assert_eq!(snap.cam(), DeviceState::Off);
        assert!(snap.ui.leave_button);
        assert_eq!(snap.others("F.O.R.G.E."), 1, "себя бот в счёт не берёт");
        assert_eq!(snap.others("  f.o.r.g.e.  "), 1, "регистр и пробелы не считаются");
    }

    /// Полупустой снимок — норма, а не ошибка разбора.
    #[test]
    fn a_half_empty_snapshot_is_still_a_snapshot() {
        let snap = PageSnapshot::parse(r#"{"perf":10}"#).unwrap();
        assert!(snap.participants.is_empty());
        assert_eq!(snap.mic(), DeviceState::Unknown, "«не знаю» не равно «выключен»");
        assert!(!snap.ended);
        assert_eq!(snap.others("бот"), 0);
    }

    /// ДЕГРАДАЦИЯ: без имён участники становятся «Участник N».
    #[test]
    fn nameless_participants_become_numbered_ones() {
        let mut namer = ParticipantNamer::new();
        let got = namer.label(&[raw(Some("a"), "", false), raw(Some("b"), "", true)]);
        assert_eq!(got[0].name, "Участник 1");
        assert_eq!(got[1].name, "Участник 2");
        assert!(got[1].speaking);
    }

    /// Номер закреплён за участником НАВСЕГДА: он уезжает в транскрипт, и
    /// «Участник 2» обязан оставаться одним и тем же человеком.
    #[test]
    fn a_number_belongs_to_a_participant_forever() {
        let mut namer = ParticipantNamer::new();
        let first = namer.label(&[raw(Some("a"), "", false), raw(Some("b"), "", false)]);
        assert_eq!(first[1].name, "Участник 2");

        // «a» вышел — «b» обязан сохранить свой номер.
        let second = namer.label(&[raw(Some("b"), "", false)]);
        assert_eq!(second[0].name, "Участник 2", "номер переиспользован");

        // Пришёл новый — он получает СЛЕДУЮЩИЙ номер, а не освободившийся.
        let third = namer.label(&[raw(Some("b"), "", false), raw(Some("c"), "", false)]);
        assert_eq!(third[1].name, "Участник 3");
        assert_eq!(namer.known(), 3);
    }

    /// Имя из DOM побеждает нумерацию, но ключ остаётся тем же.
    #[test]
    fn a_named_participant_keeps_its_name() {
        let mut namer = ParticipantNamer::new();
        let got = namer.label(&[raw(Some("p1"), "  Алиса  ", false)]);
        assert_eq!(got[0].name, "Алиса", "имя обязано быть подрезано по краям");
        assert_eq!(got[0].platform_id.as_deref(), Some("id:p1"));
    }

    /// Без атрибута ключом становится имя: у Телемоста стабильного
    /// идентификатора плитки нет вовсе (измерено Ф0).
    #[test]
    fn without_an_attribute_the_name_is_the_key() {
        let mut namer = ParticipantNamer::new();
        namer.label(&[raw(None, "Максим К.", false)]);
        let again = namer.label(&[raw(None, "максим к.", false)]);
        assert_eq!(namer.known(), 1, "тот же человек посчитан дважды");
        assert_eq!(again[0].platform_id.as_deref(), Some("name:максим к."));
    }

    /// Ось берётся ОДНИМ измерением обоих часов и дальше идёт по монотонному
    /// `performance.now()`.
    #[test]
    fn the_axis_is_anchored_once_and_stays_monotonic() {
        let mut axis = Axis::new();
        assert_eq!(axis.at_ms(5_000.0), None, "до записи оси нет");

        // Запись началась в 1700000000000; снимок сделан на 2 секунды позже,
        // и его `performance.now()` равен 7000.
        let snap = PageSnapshot {
            perf: 7_000.0,
            unix: 1_700_000_002_000.0,
            ..PageSnapshot::default()
        };
        axis.anchor(&snap, 1_700_000_000_000);
        assert!(axis.is_set());
        assert_eq!(axis.at_ms(7_000.0), Some(2_000), "момент снимка на оси");
        assert_eq!(axis.at_ms(12_000.0), Some(7_000));

        // Системные часы перевели на минуту назад — ось не шелохнулась.
        let shifted = PageSnapshot {
            perf: 20_000.0,
            unix: 1_699_999_950_000.0,
            ..PageSnapshot::default()
        };
        axis.anchor(&shifted, 1_700_000_000_000);
        assert_eq!(axis.at_ms(20_000.0), Some(15_000));
    }

    /// Снимок раньше `T0` осью не становится: отрицательных меток на общей оси
    /// не бывает по построению.
    #[test]
    fn a_snapshot_older_than_t0_never_anchors_the_axis() {
        let mut axis = Axis::new();
        let snap = PageSnapshot {
            perf: 100.0,
            unix: 1_699_999_999_000.0,
            ..PageSnapshot::default()
        };
        axis.anchor(&snap, 1_700_000_000_000);
        assert!(!axis.is_set());
        axis.anchor(&PageSnapshot::default(), 1_700_000_000_000);
        assert!(!axis.is_set(), "пустой снимок не имеет права задать ось");
    }
}
