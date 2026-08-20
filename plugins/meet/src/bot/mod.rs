//! Бот встречи: запуск Chrome, вход на платформу, наблюдение за участниками.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Chrome целиком спрятан за [`MeetBot`]
//!
//! Это главное решение модуля, и оно не про эстетику. Весь пайплайн — ingest,
//! учёт треков, атрибуция, статусы, пост-обработка — проверяется БЕЗ ЕДИНОГО
//! ЗАПУЩЕННОГО БРАУЗЕРА, потому что на месте бота стоит [`ScriptedBot`],
//! проигрывающий заданный сценарий. Тест сценария «двое говорят, перебивая
//! друг друга» идёт миллисекунды и даёт байт в байт тот же поток событий при
//! каждом прогоне; тот же тест на живом Chrome занимал бы минуту и флакал.
//!
//! Ф4 добавит `chrome.rs` — вторую реализацию того же трейта поверх
//! [`forge_cdp`]. Ни одна строка пайплайна при этом не поменяется.
//!
//! # Два решения, которые здесь нельзя переиграть
//!
//! **Селекторы — данные, а не код.** Встроенный `meet.probes.toml`
//! (`include_str!`), переопределяемый файлом `~/.config/forge/meet.probes.toml`:
//! вёрстка Meet и Teams меняется чаще, чем мы выпускаем релизы, и правка TOML
//! чинит бота БЕЗ ПЕРЕСБОРКИ. Проба Ф0 подтвердила необходимость: вёрстка
//! Телемоста собрана на CSS-модулях с хешами (`item_NZ2DW`, `TextName_BOaIg`),
//! которые меняются при каждой пересборке фронта, поэтому селекторы обязаны
//! быть подстрочными.
//!
//! **Запись НЕ ЗАВИСИТ ни от одной DOM-пробы.** Пробы влияют только на имена
//! спикеров и на детект конца встречи, у которого есть независимые сигналы:
//! все треки `ended`, URL ушёл на пост-звонковый, target упал, ручной стоп,
//! потолок четыре часа. Протухшая вёрстка деградирует до «Участник 1..N», а не
//! отменяет запись.
//!
//! # Ассистированный вход — не украшение
//!
//! [`JoinMode::Assisted`] открывает окно ВИДИМЫМ и **не делает ни одного
//! клика**, пока человек не скажет [`MeetBot::confirm_human_ready`]. Одной
//! кнопкой снимается риск отказа входа на всех платформах реестра сразу —
//! логин, капча, лобби, антибот. Поэтому режим делается в Ф3, а не «когда
//! понадобится».
//!
//! **Капчу мы не решаем автоматически ни при каких условиях.** Обнаружив её,
//! бот эмитит [`crate::hub::MeetEvent::NeedsHuman`] и отдаёт скриншот.

pub mod attrib;
pub mod chrome;
pub mod endgame;
pub mod frame;
pub mod inject;
pub mod mode;
pub mod observe;
pub mod platforms;
pub mod scripted;
pub mod transport;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::MeetError;
use crate::hub::event::{MeetEvent, ParticipantInfo, StopReason};

pub use attrib::{
    attribute, msid_participant, AttribConfig, AttributionReport, BindingStage, TrackBinding,
};
pub use chrome::{
    ensure_private_dir, hotkey_allowed, persistent_profile_dir, ChromeBot, ChromeBotConfig,
    ChromeBotFactory, Hotkey, ProfileLease,
};
pub use endgame::{EndConfig, EndSignals, EndWatch};
pub use frame::AudioFrame;
pub use mode::{detect_track_mode, TrackMode, TrackObservation};
pub use observe::{Axis, DeviceState, PageSnapshot, ParticipantNamer, RawParticipant};
pub use platforms::{ProbeBook, ProbeSet};
pub use scripted::{BotScript, ScriptStep, ScriptedBot};

/// Восьмибайтовая сигнатура PNG плюс пустой IHDR.
///
/// «Что видит бот» обязано отдавать картинку даже тогда, когда браузера ещё
/// нет: именно в этот момент скриншот и нужен больше всего — человек смотрит,
/// почему бот не вошёл. Отказ вместо картинки означал бы пустое место в
/// интерфейсе без единого объяснения.
pub const PLACEHOLDER_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // сигнатура PNG
    0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', // длина + тип чанка
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
];

/// Как бот входит во встречу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JoinMode {
    /// Бот входит сам: гасит микрофон и камеру, вводит имя, жмёт «войти».
    #[default]
    Normal,
    /// Бот открывает окно видимым и **ничего не кликает**. Логин, капчу или
    /// лобби проходит человек и жмёт «я внутри — пиши»
    /// ([`MeetBot::confirm_human_ready`]).
    ///
    /// Инжект при этом уже стоит: он ставится ДО навигации, поэтому запись
    /// стартует немедленно после подтверждения, не переоткрывая страницу.
    Assisted,
}

impl JoinMode {
    /// Значение колонки `meetings.mode`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Assisted => "assisted",
        }
    }

    /// Разбор параметра запроса. `None` — значение не из этого перечисления.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "assisted" => Some(Self::Assisted),
            _ => None,
        }
    }

    /// Показывается ли окно человеку сразу при старте.
    pub fn starts_visible(self) -> bool {
        matches!(self, Self::Assisted)
    }
}

/// Что бот видит во встрече прямо сейчас.
///
/// Снимок, а не поток: наблюдение дёргается по таймеру, и хранить между
/// вызовами нечего.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observation {
    /// Момент наблюдения на общей оси, мс от `T0`.
    pub at_ms: u64,
    /// Участники по DOM-пробе. Пусто — проба протухла либо ещё не сработала;
    /// это НЕ ошибка, деградация идёт до «Участник N».
    pub participants: Vec<ParticipantInfo>,
    /// Кого платформа считает говорящим. `None` — тишина либо маркера нет.
    pub active_speaker: Option<String>,
    /// Наблюдение за треками — вход [`detect_track_mode`].
    pub tracks: TrackObservation,
}

/// Итог показа окна человеку.
///
/// Возвращаются ФАКТИЧЕСКИЕ границы, а не запрошенные: оконный сервер macOS
/// вправе подрезать координаты (измерено в Ф1 — запрос `left: -4000`
/// превращается в `left: -930`), и разница между «попросили» и «получилось» —
/// это то, что ассистированному входу надо знать, а не предполагать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowReport {
    /// Просили показать или спрятать.
    pub visible: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// Каналы, по которым бот говорит с рекордером.
///
/// Два разных канала, а не один, потому что у них разная природа нагрузки.
/// События — единицы в секунду, их терять нельзя (неограниченная очередь).
/// Аудиокадры — 250 в секунду на пять треков, и здесь очередь ОГРАНИЧЕНА:
/// неограниченная при вставшем писателе съела бы память, а backpressure на
/// приёме из CDP — это ровно то, что нужно (Chrome подождёт).
pub struct BotStreams {
    /// События жизненного цикла.
    pub events: mpsc::UnboundedReceiver<MeetEvent>,
    /// Пер-трековый PCM.
    pub audio: mpsc::Receiver<AudioFrame>,
}

/// Ёмкость канала аудиокадров.
///
/// 512 кадров по 20 мс это десять секунд звука одного трека — с запасом
/// перекрывает любую паузу писателя на диске и при этом не даёт очереди расти
/// бесконечно, если диск встал совсем.
pub const AUDIO_CHANNEL_CAPACITY: usize = 512;

/// Бот встречи.
///
/// Единственная абстракция, за которой прячется Chrome. Реализаций две:
/// [`ScriptedBot`] (тесты) и `ChromeBot` поверх [`forge_cdp`] (Ф4).
#[async_trait]
pub trait MeetBot: Send + Sync {
    /// Запускает браузер, ставит инжект ДО навигации и открывает встречу.
    ///
    /// Возвращается СРАЗУ, отдав каналы: вход может занять минуты
    /// (ассистированный режим ждёт человека), и держать вызывающего всё это
    /// время значило бы не показывать ему ни статусов, ни скриншотов.
    ///
    /// Звать можно один раз: каналы отдаются по значению.
    async fn start(&self, mode: JoinMode) -> Result<BotStreams, MeetError>;

    /// «Я внутри — пиши». Осмысленно только в [`JoinMode::Assisted`].
    ///
    /// В [`JoinMode::Normal`] возвращает `Ok(())` и ничего не делает: повторное
    /// подтверждение не должно быть ошибкой, иначе фронт был бы обязан знать
    /// режим встречи, чтобы не нажать лишнюю кнопку.
    async fn confirm_human_ready(&self) -> Result<(), MeetError>;

    /// Выключает микрофон и камеру бота.
    ///
    /// Второй барьер поверх `--use-file-for-fake-audio-capture`: файл тишины
    /// не даёт боту пикать, а эта кнопка — быть услышанным вообще. Два
    /// независимых барьера, потому что цена ошибки — бот, гудящий в чужую
    /// встречу.
    async fn set_media(&self, mic: bool, cam: bool) -> Result<(), MeetError>;

    /// Снимок состояния встречи.
    async fn observe(&self) -> Result<Observation, MeetError>;

    /// Показывает окно бота человеку либо убирает его обратно.
    ///
    /// **Единственное место, которому позволено отобрать фокус** — и зовётся
    /// оно только по явному действию человека («покажи, что там у бота»).
    async fn show_window(&self, visible: bool) -> Result<WindowReport, MeetError>;

    /// PNG «что видит бот».
    ///
    /// Не имеет права падать на ещё не загруженной странице: именно в этот
    /// момент он и нужен больше всего — человек смотрит, почему бот не вошёл.
    async fn screenshot(&self) -> Result<Vec<u8>, MeetError>;

    /// Выходит из встречи и гасит браузер.
    ///
    /// Идемпотентна: сюда приходят и по команде пользователя, и по
    /// автовыходу, и из graceful shutdown — все три могут совпасть.
    async fn stop(&self, reason: StopReason) -> Result<(), MeetError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MeetBot` обязан быть ОБЪЕКТНО-БЕЗОПАСНЫМ: рекордер держит
    /// `Arc<dyn MeetBot>` и не знает, живой это Chrome или сценарий. Уроните
    /// объектную безопасность (`async fn` без `#[async_trait]`, generic-метод)
    /// — и Ф4 упрётся в переписывание всего пайплайна.
    #[tokio::test]
    async fn the_trait_is_object_safe() {
        let bot: std::sync::Arc<dyn MeetBot> =
            std::sync::Arc::new(ScriptedBot::new(BotScript::default()));
        bot.set_media(false, false).await.unwrap();
        bot.stop(StopReason::UserStop).await.unwrap();
        assert!(!bot.screenshot().await.unwrap().is_empty());
    }

    #[test]
    fn join_mode_has_one_string_form_and_only_assisted_shows_the_window() {
        assert_eq!(JoinMode::Normal.as_str(), "normal");
        assert_eq!(JoinMode::Assisted.as_str(), "assisted");
        assert_eq!(JoinMode::parse("assisted"), Some(JoinMode::Assisted));
        assert_eq!(JoinMode::parse("  NORMAL "), Some(JoinMode::Normal));
        assert_eq!(JoinMode::parse("headless"), None, "чужой режим не молчит");

        assert!(JoinMode::Assisted.starts_visible());
        assert!(!JoinMode::Normal.starts_visible());

        // serde-форма обязана совпасть со строкой в колонке `meetings.mode`:
        // разойдись они, фронт фильтровал бы по значению, которого в базе нет.
        for m in [JoinMode::Normal, JoinMode::Assisted] {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(json, format!("\"{}\"", m.as_str()));
        }
    }
}
