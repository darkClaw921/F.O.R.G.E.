//! [`ChromeBot`] — боевой бот поверх [`forge_cdp`].
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Что здесь происходит
//!
//! ```text
//! launch(Chrome) → WsTransport → CdpPage::open → подписка на биндинг
//!   → prepare_and_navigate (инжект ДО навигации)
//!     → вход по пробам (или ожидание человека)
//!       ├── насос: биндинг → MeetEvent + AudioFrame
//!       └── наблюдение: пробы → участники, спикер, конец встречи
//! ```
//!
//! Две задачи, а не одна, и это несущее решение. Насос обязан крутиться
//! ВСЕГДА — в том числе пока человек проходит капчу, — потому что инжект уже
//! стоит и звук пойдёт с первой же дорожки. Наблюдение же ходит в DOM раз в
//! секунду и вправе промахиваться сколько угодно: **запись от него не
//! зависит**.
//!
//! # Порядок команд CDP нарушать нельзя
//!
//! `setBypassCSP` → `grantPermissions` → `addBinding` →
//! `addScriptToEvaluateOnNewDocument` → `setAutoAttach` → `navigate`.
//! Живая проба Ф0 показала прямым тестом: CSP Телемоста блокирует инлайн-скрипт
//! (`cspBlocksInlineScript: true`). Инжект после навигации бесполезен —
//! `RTCPeerConnection` страницы уже создан, патчить нечего. Порядок держит
//! [`forge_cdp::CdpPage::prepare_and_navigate`], а здесь на него есть тест
//! через фейковый транспорт: молчаливую поломку такого рода иначе замечают
//! только на живой встрече.
//!
//! # Клики — по вычисленному боксу, а не по координатам со скриншота
//!
//! На живой пробе Ф0 клик по координатам, снятым со скриншота, промахнулся:
//! скриншот отмасштабирован относительно вьюпорта.
//! [`forge_cdp::CdpPage::click_expression`] берёт `getBoundingClientRect` и
//! шлёт `Input.dispatchMouseEvent` — настоящее событие с `isTrusted`.
//!
//! # Микрофон гасится ДВАЖДЫ
//!
//! Первый барьер — флаги запуска (`--use-fake-device-for-media-stream` плюс
//! `--use-file-for-fake-audio-capture=<тишина>`): без файла тишины фейковое
//! устройство Chrome генерирует периодический БИП-ТОН, и бот пикал бы в живую
//! встречу. Второй — кнопка в UI по пробам, и она жмётся ПОСЛЕ ВХОДА, а не
//! только на пре-join: Ф0 измерила, что Телемост включает микрофон сам в
//! момент подключения.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use forge_cdp::launch::{launch, ChromeProcess, LaunchConfig};
use forge_cdp::page::{CdpPage, PageSetup, ScreenshotOptions};
use forge_cdp::session::CdpBrowser;
use forge_cdp::transport::TransportConfig;
use forge_cdp::ws::WsTransport;
use tokio::sync::{mpsc, Notify, RwLock};
use tokio_util::sync::CancellationToken;

use super::endgame::{EndConfig, EndSignals, EndWatch};
use super::inject;
use super::observe::{Axis, DeviceState, PageSnapshot, ParticipantNamer};
use super::platforms::{observe_expression, probe, probe_element_expression, ProbeBook, ProbeSet};
use super::scripted::BotAction;
use super::transport::{AudioTransport as _, CdpBindingTransport, PageMessage};
use super::{
    AudioFrame, BotStreams, JoinMode, MeetBot, Observation, TrackObservation, WindowReport,
    AUDIO_CHANNEL_CAPACITY, PLACEHOLDER_PNG,
};
use crate::config::MeetConfig;
use crate::error::MeetError;
use crate::hub::event::{MeetEvent, StopReason};
use crate::recording::{BotFactory, BotRequest};

/// Перевод режима окна из конфига плагина в режим запуска браузера.
///
/// Два перечисления с одинаковыми именами живут в разных крейтах намеренно:
/// `forge-cdp` ничего не знает о встречах, а плагин — о ключах командной
/// строки Chrome.
pub fn cdp_window_mode(mode: crate::config::WindowMode) -> forge_cdp::launch::WindowMode {
    use crate::config::WindowMode as Cfg;
    use forge_cdp::launch::WindowMode as Cdp;
    match mode {
        Cfg::Offscreen => Cdp::Offscreen,
        Cfg::Minimized => Cdp::Minimized,
        Cfg::Visible => Cdp::Visible,
        Cfg::Headless => Cdp::Headless,
    }
}

// ---------------------------------------------------------------------------
// Постоянный профиль браузера
// ---------------------------------------------------------------------------

/// Каталог постоянного профиля. Один на все встречи, живёт в кэше плагина.
pub fn persistent_profile_dir(cache_dir: &std::path::Path) -> PathBuf {
    cache_dir.join("browser-profile")
}

/// Создаёт каталог с правами ТОЛЬКО ДЛЯ ВЛАДЕЛЬЦА и проверяет их у уже
/// существующего.
///
/// Не косметика: внутри лежит живая сессия Google-аккаунта. Каталог, созданный
/// с `umask` по умолчанию, читается любым процессом пользователя и любым
/// другим пользователем машины — то есть «войдите один раз руками» означало бы
/// «положите пароль в общедоступное место».
///
/// Права выставляются и СУЩЕСТВУЮЩЕМУ каталогу: он мог быть создан прежней
/// версией плагина или распакован из бэкапа.
pub fn ensure_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

/// Занят ли постоянный профиль прямо сейчас.
///
/// **Chrome не умеет две своих копии на одном `--user-data-dir`**: второй
/// запуск либо отдаёт управление первому, либо падает. Записей же может идти
/// несколько разом (`max_concurrent_recordings`), и без этого замка вторая
/// встреча ломала бы первую.
static PERSISTENT_PROFILE_BUSY: AtomicBool = AtomicBool::new(false);

/// Аренда постоянного профиля на одну встречу.
///
/// Не получил — не беда: бот берёт одноразовый профиль и входит гостем. Это
/// ХУЖЕ (встречи с ограниченным доступом останутся недоступны), но это работает
/// и никого не ломает, в отличие от двух Chrome на одном каталоге.
#[derive(Debug)]
pub struct ProfileLease {
    granted: bool,
}

impl ProfileLease {
    /// Пытается занять профиль. `wanted = false` — аренда не нужна вовсе.
    pub fn acquire(wanted: bool) -> Self {
        let granted = wanted
            && PERSISTENT_PROFILE_BUSY
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if wanted && !granted {
            tracing::warn!(
                target: "forge_meet",
                "forge-meet: постоянный профиль занят другой записью — вхожу одноразовым гостем"
            );
        }
        Self { granted }
    }

    pub fn granted(&self) -> bool {
        self.granted
    }
}

impl Drop for ProfileLease {
    fn drop(&mut self) {
        if self.granted {
            PERSISTENT_PROFILE_BUSY.store(false, Ordering::SeqCst);
        }
    }
}

/// Тайминги адаптера. Поля, а не константы, по той же причине, что и
/// таймауты автовыхода: тест обязан проверять тот же код за сотни
/// миллисекунд, а не за минуты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTimings {
    /// Сколько всего отводится на вход, мс. Не уложились — `needs_human`, а не
    /// провал: дальше подхватывает ассистированный вход.
    pub join_timeout_ms: u64,
    /// Пауза после клика: интерфейсу нужно перерисоваться.
    pub step_settle_ms: u64,
    /// Период опроса DOM.
    pub observe_interval_ms: u64,
    /// Сколько ждать после клика «выйти», прежде чем гасить браузер.
    pub leave_grace_ms: u64,
    /// Отсрочка `SIGTERM` → `SIGKILL` группе Chrome.
    pub terminate_grace_ms: u64,
    /// Сколько подряд неудачных наблюдений считать упавшим target'ом.
    pub observe_failures_to_give_up: u32,
    /// Сколько раз жать ОДНУ И ТУ ЖЕ кнопку входа, прежде чем идти дальше.
    pub join_step_attempts: u32,
    /// Сколько раз жать «подключить звук компьютера» (Zoom).
    pub join_audio_attempts: u32,
}

impl Default for BotTimings {
    fn default() -> Self {
        Self {
            // Минута: интерстишал, ввод имени и вход укладываются в секунды, а
            // запас нужен на холодный профиль и медленную сеть.
            join_timeout_ms: 60_000,
            step_settle_ms: 700,
            observe_interval_ms: 1_000,
            leave_grace_ms: 800,
            terminate_grace_ms: 5_000,
            observe_failures_to_give_up: 3,
            // Три: первый клик мог уйти в ещё не смонтированную кнопку, второй
            // — совпасть с перерисовкой. Четвёртый уже ничего не изменит, а
            // время до `needs_human` съест.
            join_step_attempts: 3,
            // Веб-клиент Zoom рисует диалог звука не мгновенно после входа,
            // поэтому попыток чуть больше: цена промаха здесь — запись без
            // звука вообще, а цена лишней попытки — секунда.
            join_audio_attempts: 5,
        }
    }
}

/// Всё, что нужно живому боту для одной встречи.
#[derive(Debug, Clone)]
pub struct ChromeBotConfig {
    /// Канонический URL комнаты.
    pub url: String,
    /// Каталог встречи: профиль Chrome, `chrome.pid`, WAV, журнал.
    pub meeting_dir: PathBuf,
    /// Кэш плагина: там лежит общий на все встречи файл тишины.
    pub cache_dir: PathBuf,
    /// Имя, под которым бот виден участникам.
    pub bot_name: String,
    /// Пробы платформы. Пустой набор — законное состояние.
    pub probes: ProbeSet,
    /// Инжект: имя биндинга, кольцевой буфер, скрипт захвата.
    pub inject: forge_cdp::InjectConfig,
    pub sample_rate: u32,
    pub window_mode: crate::config::WindowMode,
    pub chrome_path: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
    pub mute_audio: bool,
    pub cdp_command_timeout_ms: u64,
    /// Просят ли постоянный профиль. Фактическая аренда решается
    /// [`ProfileLease`]: профиль один, а записей может идти несколько.
    pub persist_profile: bool,
    pub end: EndConfig,
    pub timings: BotTimings,
}

impl ChromeBotConfig {
    /// Собирает конфигурацию бота из конфига плагина и заявки.
    pub fn new(cfg: &MeetConfig, req: &BotRequest, probes: ProbeSet) -> Self {
        Self {
            url: req.url.clone(),
            meeting_dir: req.meeting_dir.clone(),
            cache_dir: cfg.cache_dir.clone(),
            bot_name: req.bot_name.clone(),
            probes,
            inject: inject::inject_config(cfg),
            sample_rate: cfg.audio_sample_rate,
            window_mode: cfg.window_mode,
            chrome_path: cfg.chrome_path.clone(),
            ffmpeg_path: cfg.ffmpeg_path.clone(),
            mute_audio: cfg.mute_audio,
            cdp_command_timeout_ms: cfg.cdp_command_timeout_ms,
            persist_profile: cfg.persist_profile,
            end: EndConfig::from(cfg),
            timings: BotTimings::default(),
        }
    }

    /// Параметры запуска браузера.
    ///
    /// `persistent` приходит параметром, а не берётся из поля: профиль один на
    /// весь плагин, и решение «этой встрече он достался» принимает
    /// [`ProfileLease`], а не конфигурация. Не досталось — обычный одноразовый
    /// профиль в каталоге встречи, ровно как до Ф8.
    pub fn launch_config(&self, persistent: bool) -> LaunchConfig {
        let mut launch_cfg = LaunchConfig::new(&self.meeting_dir, &self.cache_dir);
        launch_cfg.window_mode = cdp_window_mode(self.window_mode);
        launch_cfg.chrome_binary = self.chrome_path.clone();
        launch_cfg.ffmpeg_binary = self.ffmpeg_path.clone();
        launch_cfg.mute_audio = self.mute_audio;
        if persistent {
            launch_cfg.user_data_dir = persistent_profile_dir(&self.cache_dir);
        }
        launch_cfg
    }

    /// Параметры подготовки страницы.
    pub fn page_setup(&self, mode: JoinMode) -> PageSetup {
        PageSetup {
            inject: self.inject.clone(),
            // Ассистированный вход показывает окно сразу: человек обязан
            // видеть, что происходит, ещё до первого клика.
            window_mode: if mode.starts_visible() {
                forge_cdp::launch::WindowMode::Visible
            } else {
                cdp_window_mode(self.window_mode)
            },
            ..PageSetup::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Вход: чистое решение отдельно от кликов
// ---------------------------------------------------------------------------

/// Что делать на текущем экране.
///
/// Решение вынесено ЧИСТОЙ функцией [`next_join_step`] намеренно: вход — самая
/// ветвистая часть адаптера (интерстишал, гость, залогиненный профиль, лобби,
/// капча), и проверять её прогонами браузера означало бы не проверять вовсе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStep {
    /// Жать «Продолжить в браузере».
    ContinueInBrowser,
    /// Вводить имя бота — ГОСТЕВАЯ ветка pre-join.
    TypeName,
    /// Гасить микрофон и камеру ДО входа.
    ///
    /// Шаг появился в Ф13. До него устройства гасились только ПОСЛЕ входа
    /// (шаг 7 адаптера), то есть бот на несколько секунд въезжал в чужую
    /// встречу с живым микрофоном и работающей камерой. На пре-джойне
    /// vcc.airlabs.pro обе кнопки есть и состояние читается однозначно —
    /// значит, платить этими секундами незачем.
    ///
    /// Шаг берётся ОДИН РАЗ за вход ([`JoinProgress::media_muted`]): кнопка,
    /// не сработавшая с трёх попыток `ensure_device`, не имеет права
    /// съедать весь таймаут входа.
    MuteDevices,
    /// Жать «войти».
    Join,
    /// Лобби: ждём, пока впустят.
    Lobby,
    /// Капча. **Не решаем никогда** — зовём человека.
    Captcha,
    /// Площадка ОТКАЗАЛА во входе. Ждать бесполезно — зовём человека сразу.
    Denied,
    /// Встречи БОЛЬШЕ НЕТ: комната закрыта либо ссылка недействительна.
    ///
    /// Отдельный шаг, а не «ничего знакомого на экране», по той же причине,
    /// что и [`JoinStep::Denied`]: мёртвая комната vcc.airlabs.pro отдаёт
    /// страницу «Встреча завершена» мгновенно, без пре-джойна, и без этого
    /// шага бот минуту молотил бы в пустоту, а потом звал человека с причиной
    /// `unknown`. Человеку при этом делать нечего: встреча кончилась.
    Ended,
    /// Мы внутри встречи.
    Inside,
    /// Ничего знакомого на экране: ждём следующего наблюдения.
    Wait,
}

/// Ход входа: что уже сделано и сколько раз.
///
/// Счётчики попыток — не украшение, а лекарство от зацикливания, найденное
/// ЖИВЫМ ПРОГОНОМ. Кнопка, нажатая ботом, вовсе не обязана исчезать: площадка
/// вправе оставить её в разметке (мок-встреча просто ставит `disabled`).
/// Без ограничения попыток бот жал «Продолжить в браузере» по кругу все
/// двадцать секунд таймаута и ни разу не добрался до поля имени — а выглядело
/// это как «вход не работает», без единой ошибки в логе.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JoinProgress {
    /// Имя бота уже введено.
    pub name_typed: bool,
    /// Устройства уже гасили на пре-джойне.
    pub media_muted: bool,
    /// Сколько раз жали интерстишал.
    pub interstitial_attempts: u32,
    /// Сколько раз жали «войти».
    pub join_attempts: u32,
}

/// Следующий шаг входа по снимку страницы.
///
/// Порядок проверок и есть содержание функции:
///
/// 1. **капча** — раньше всего, иначе бот будет молотить по кнопке, которой
///    нет, пока не выйдет таймаут;
/// 2. **мы внутри** (кнопка выхода) — иначе бот жал бы «войти» второй раз уже
///    во встрече;
/// 3. **встреча уже кончилась** — тоже ПОСЛЕ «мы внутри» и ровно по той же
///    причине: маркер «Встреча завершена» живёт в том же документе, что и
///    панель, и протухший селектор иначе выкидывал бы бота с живой встречи;
/// 4. **отказ во входе** — ПОСЛЕ проверки «мы внутри» и это не придирка:
///    маркер отказа живёт в том же документе, что и панель встречи, и
///    протухший селектор, поймавший что-нибудь во время звонка, иначе выкинул
///    бы бота с ЖИВОЙ встречи. Раз кнопка выхода видна — мы внутри, и никакой
///    текст этого не отменяет;
/// 5. интерстишал → имя → **гашение устройств** → вход, каждый шаг не больше
///    `max_attempts` раз;
/// 6. **лобби** ПОСЛЕ входа: маркер ожидания появляется именно там.
///
/// Две ветки pre-join различаются НАЛИЧИЕМ ПОЛЯ ИМЕНИ, а не отдельной пробой
/// «залогинен»: Ф0 измерила, что на залогиненном профиле поля нет вовсе, а
/// протухший маркер залогиненности отправлял бы бота не в ту ветку молча.
///
/// # Почему отказ — отдельный шаг, а не «просто не дождались»
///
/// Google Meet при ограниченном доступе хоста отклоняет заявку анонимного
/// участника АВТОМАТИЧЕСКИ и за секунды. Без отдельного шага бот стоял бы
/// минуту до таймаута и звал человека с причиной `unknown` — то есть
/// продуктовое ограничение площадки выглядело бы нашей поломкой, и человек
/// искал бы дефект там, где его нет.
pub fn next_join_step(snap: &PageSnapshot, progress: &JoinProgress, max_attempts: u32) -> JoinStep {
    if snap.ui.captcha {
        return JoinStep::Captcha;
    }
    if snap.ui.leave_button {
        return JoinStep::Inside;
    }
    if snap.ended {
        return JoinStep::Ended;
    }
    if snap.denied {
        return JoinStep::Denied;
    }
    if snap.ui.continue_in_browser && progress.interstitial_attempts < max_attempts {
        return JoinStep::ContinueInBrowser;
    }
    if snap.ui.name_input && !progress.name_typed {
        return JoinStep::TypeName;
    }
    // Устройства гасятся ДО клика «войти» и только когда состояние ИЗВЕСТНО и
    // равно «включено». «Не знаю» шага не рождает: у площадки без кнопок
    // пре-джойна (Телемост) снимок молчит, и вход обязан идти как прежде.
    if !progress.media_muted
        && (snap.mic() == DeviceState::On || snap.cam() == DeviceState::On)
    {
        return JoinStep::MuteDevices;
    }
    if snap.ui.join_button && progress.join_attempts < max_attempts {
        return JoinStep::Join;
    }
    if snap.ui.lobby {
        return JoinStep::Lobby;
    }
    JoinStep::Wait
}

/// Чем кончился вход.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JoinOutcome {
    Inside,
    NeedsHuman(String),
    /// Встречи уже нет: комната закрыта либо ссылка недействительна.
    ///
    /// **Не провал и не «нужен человек».** Записывать нечего, звать некого,
    /// и единственный честный исход — обычный конец встречи с причиной
    /// [`StopReason::DomEnded`]. Провалом это стало бы только по общему
    /// правилу плагина: `failed` ставится, когда нет транскрипта, а не когда
    /// бот чего-то не смог.
    Ended,
}

// ---------------------------------------------------------------------------
// Горячие клавиши устройств
// ---------------------------------------------------------------------------

/// Разобранное сочетание клавиш: модификаторы битовой маской CDP и сама клавиша.
///
/// Маска ровно та, которую ждёт `Input.dispatchKeyEvent`: 1 = Alt, 2 = Ctrl,
/// 4 = Meta/Command, 8 = Shift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    /// Одна печатная клавиша в нижнем регистре: `d`, `e`, `a`, `1`.
    pub key: char,
}

impl Hotkey {
    /// Разбирает `ctrl+d`, `alt+a`, `ctrl+shift+m`.
    ///
    /// Возвращает `None` на всём, что не разобралось: сочетание приезжает из
    /// TOML, который правит человек, и опечатка обязана стоить одной пробы, а
    /// не встречи. Молчаливое «нажму что-нибудь похожее» здесь недопустимо —
    /// это переключатель микрофона.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut modifiers = 0u32;
        let mut key: Option<char> = None;
        for part in raw.split('+') {
            let part = part.trim().to_ascii_lowercase();
            if part.is_empty() {
                return None;
            }
            match part.as_str() {
                "alt" | "option" => modifiers |= 1,
                "ctrl" | "control" => modifiers |= 2,
                "meta" | "cmd" | "command" | "super" => modifiers |= 4,
                "shift" => modifiers |= 8,
                other => {
                    // Клавиша ровно одна и ровно одна: `ctrl+d+e` — это
                    // опечатка, а не «нажми обе».
                    let mut chars = other.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() || key.is_some() || !c.is_ascii_alphanumeric() {
                        return None;
                    }
                    key = Some(c);
                }
            }
        }
        // Голая буква без модификаторов — почти наверняка описка, а «нажать
        // просто D» в поле ввода имени написало бы букву в имя бота.
        let key = key?;
        if modifiers == 0 {
            return None;
        }
        Some(Self { modifiers, key })
    }

    /// `Input.dispatchKeyEvent` требует и `key`, и `code`, и `windowsVirtualKeyCode`.
    fn code(&self) -> String {
        if self.key.is_ascii_digit() {
            format!("Digit{}", self.key)
        } else {
            format!("Key{}", self.key.to_ascii_uppercase())
        }
    }

    fn vk(&self) -> u32 {
        self.key.to_ascii_uppercase() as u32
    }
}

/// Можно ли слать сочетание клавиш при таком состоянии устройства.
///
/// **СОЧЕТАНИЕ — ЭТО ПЕРЕКЛЮЧАТЕЛЬ, А НЕ «ВЫКЛЮЧИТЬ».** `Ctrl+D` у Google Meet
/// не «выключи микрофон», а «поменяй местами» — и на неизвестном состоянии он
/// с равной вероятностью ВКЛЮЧИТ микрофон боту, сидящему в чужой встрече.
/// Поэтому правило простое и не подлежит смягчению: сочетание отправляется
/// ТОЛЬКО когда состояние ИЗВЕСТНО и ПРОТИВОПОЛОЖНО желаемому.
///
/// Когда состояние неизвестно (пробы кнопок промахнулись), остаётся клик по
/// пробе, а последним рубежом — флаги запуска Chrome: фейковое устройство с
/// файлом тишины не даёт боту зазвучать даже с включённым микрофоном.
pub fn hotkey_allowed(want_on: bool, state: DeviceState) -> bool {
    matches!(
        (want_on, state),
        (true, DeviceState::Off) | (false, DeviceState::On)
    )
}

// ---------------------------------------------------------------------------
// Общее состояние бота
// ---------------------------------------------------------------------------

/// Состояние, разделяемое методами трейта и фоновыми задачами.
struct Shared {
    page: RwLock<Option<Arc<CdpPage>>>,
    chrome: Mutex<Option<ChromeProcess>>,
    cancel: CancellationToken,
    human_ready: AtomicBool,
    human_notify: Notify,
    started: AtomicBool,
    stopped: AtomicBool,
    finished: AtomicBool,
    requested: Mutex<Option<StopReason>>,
    actions: Mutex<Vec<BotAction>>,
    namer: Mutex<ParticipantNamer>,
    axis: Mutex<Axis>,
    last: RwLock<Observation>,
    /// `T0` записи в unix-миллисекундах — приезжает из страницы.
    t0_unix: AtomicI64,
    tracks_seen: AtomicUsize,
    tracks_live: AtomicUsize,
    speaking_tracks: AtomicUsize,
    /// VAD хоть раз за встречу нашёл речь.
    ///
    /// Отдельный флаг рядом со счётчиком говорящих дорожек, а не «счётчик был
    /// когда-то ненулевым»: счётчик показывает СЕЙЧАС и обнуляется на каждой
    /// паузе, а страж глухоты ([`super::medialog::DeafWatch`]) спрашивает про
    /// всю встречу.
    speech_seen: AtomicBool,
    /// Участники находились хоть раз. См. страж «протухшие пробы не выкидывают
    /// бота» в [`super::endgame`].
    participants_known: AtomicBool,
    /// Событие `joined` уже отправлено.
    joined: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            page: RwLock::new(None),
            chrome: Mutex::new(None),
            cancel: CancellationToken::new(),
            human_ready: AtomicBool::new(false),
            human_notify: Notify::new(),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            requested: Mutex::new(None),
            actions: Mutex::new(Vec::new()),
            namer: Mutex::new(ParticipantNamer::new()),
            axis: Mutex::new(Axis::new()),
            last: RwLock::new(Observation::default()),
            t0_unix: AtomicI64::new(0),
            tracks_seen: AtomicUsize::new(0),
            tracks_live: AtomicUsize::new(0),
            speaking_tracks: AtomicUsize::new(0),
            speech_seen: AtomicBool::new(false),
            participants_known: AtomicBool::new(false),
            joined: AtomicBool::new(false),
        }
    }

    fn note(&self, action: BotAction) {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }

    /// Отправляет `joined` РОВНО ОДИН РАЗ.
    ///
    /// Звать его умеют двое, и оба правы. Цикл входа знает, что нашёл кнопку
    /// выхода; насос знает, что со страницы пришла первая дорожка, — а
    /// дорожка удалённого участника означает, что бот уже во встрече.
    ///
    /// ЖИВОЙ ПРОГОН ПОКАЗАЛ, ЧТО ЭТО НЕ ТЕОРИЯ: страница успевала отдать
    /// `recording_started` раньше, чем цикл входа делал следующее наблюдение,
    /// и поток событий получался `recording_started` → `joined`, то есть
    /// возвращался на предыдущую стадию. Контракт порядка это запрещает, а
    /// статус-машина в таком порядке пыталась откатить `recording` в
    /// `joining`.
    fn emit_joined_once(&self, events: &mpsc::UnboundedSender<MeetEvent>) {
        if !self.joined.swap(true, Ordering::SeqCst) {
            let _ = events.send(MeetEvent::Joined);
        }
    }

    async fn page(&self) -> Option<Arc<CdpPage>> {
        self.page.read().await.clone()
    }

    /// Гасит браузер. **Идемпотентна**: сюда приходят и по команде человека, и
    /// по автовыходу, и из graceful shutdown — все три могут совпасть.
    async fn terminate(&self, grace: Duration) {
        let chrome = self
            .chrome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut chrome) = chrome {
            let _ = chrome.terminate(grace).await;
        }
    }
}

/// Боевой бот встречи.
pub struct ChromeBot {
    cfg: Arc<ChromeBotConfig>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ChromeBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeBot")
            .field("url", &self.cfg.url)
            .field("probes", &self.cfg.probes.filled())
            .field("started", &self.shared.started.load(Ordering::SeqCst))
            .finish()
    }
}

impl ChromeBot {
    pub fn new(cfg: ChromeBotConfig) -> Self {
        Self {
            cfg: Arc::new(cfg),
            shared: Arc::new(Shared::new()),
        }
    }

    /// Что бот сделал со страницей, по порядку.
    ///
    /// Существует ради одного инварианта, который нельзя проверить чтением
    /// кода: **в ассистированном режиме бот не делает ни одного клика до
    /// подтверждения человеком**.
    pub fn actions(&self) -> Vec<BotAction> {
        self.shared
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Сколько раз бот кликал и печатал.
    pub fn click_count(&self) -> usize {
        self.actions()
            .iter()
            .filter(|a| matches!(a, BotAction::Click(_) | BotAction::Type(_)))
            .count()
    }

    /// Закончила ли фоновая задача работу.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::SeqCst)
    }

    pub fn config(&self) -> &ChromeBotConfig {
        &self.cfg
    }

    /// Выполняет выражение на странице бота и отдаёт результат.
    ///
    /// Диагностический вход, а не рабочий путь: им пользуются живые тесты
    /// (спросить у мок-встречи, что именно нажали) и разбор полётов. Боевая
    /// логика ходит в страницу только через пробы.
    pub async fn evaluate_json(&self, expression: &str) -> Result<serde_json::Value, MeetError> {
        let page = self
            .shared
            .page()
            .await
            .ok_or_else(|| MeetError::Internal("браузер ещё не запущен".to_string()))?;
        let res = page
            .session()
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expression, "returnByValue": true}),
            )
            .await
            .map_err(|e| MeetError::Internal(format!("выражение не выполнилось: {e}")))?;
        Ok(res["result"]["value"].clone())
    }
}

#[async_trait]
impl MeetBot for ChromeBot {
    async fn start(&self, mode: JoinMode) -> Result<BotStreams, MeetError> {
        if self.shared.started.swap(true, Ordering::SeqCst) {
            return Err(MeetError::Internal(
                "ChromeBot уже запущен: каналы отдаются один раз".to_string(),
            ));
        }
        let (etx, erx) = mpsc::unbounded_channel();
        let (atx, arx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);

        let _ = etx.send(MeetEvent::Queued);
        let _ = etx.send(MeetEvent::Launching);

        let cfg = self.cfg.clone();
        let shared = self.shared.clone();
        tokio::spawn(async move {
            run(cfg, shared, mode, etx, atx).await;
        });

        Ok(BotStreams {
            events: erx,
            audio: arx,
        })
    }

    async fn confirm_human_ready(&self) -> Result<(), MeetError> {
        // Повторное подтверждение — не ошибка: фронт не обязан знать режим
        // встречи, чтобы не нажать лишнюю кнопку.
        if self.shared.human_ready.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.shared.note(BotAction::HumanConfirmed);
        self.shared.human_notify.notify_waiters();
        Ok(())
    }

    async fn set_media(&self, mic: bool, cam: bool) -> Result<(), MeetError> {
        self.shared.note(BotAction::SetMedia { mic, cam });
        let Some(page) = self.shared.page().await else {
            return Ok(());
        };
        apply_media(&self.cfg, &self.shared, &page, mic, cam).await;
        Ok(())
    }

    async fn observe(&self) -> Result<Observation, MeetError> {
        let Some(page) = self.shared.page().await else {
            return Ok(self.shared.last.read().await.clone());
        };
        match snapshot(&self.cfg, &self.shared, &page).await {
            Ok((_, obs)) => Ok(obs),
            // Промах наблюдения — не ошибка встречи: отдаём последнее, что
            // видели. Ошибкой это было бы, только если бы от наблюдения
            // зависела запись, а она не зависит.
            Err(_) => Ok(self.shared.last.read().await.clone()),
        }
    }

    async fn show_window(&self, visible: bool) -> Result<WindowReport, MeetError> {
        self.shared.note(BotAction::ShowWindow(visible));
        let page = self
            .shared
            .page()
            .await
            .ok_or_else(|| MeetError::Internal("браузер ещё не запущен".to_string()))?;
        let bounds = page
            .set_window_visible(visible)
            .await
            .map_err(|e| MeetError::Internal(format!("окно не переставилось: {e}")))?;
        Ok(WindowReport {
            visible,
            left: bounds.left,
            top: bounds.top,
            width: bounds.width,
            height: bounds.height,
        })
    }

    async fn screenshot(&self) -> Result<Vec<u8>, MeetError> {
        self.shared.note(BotAction::Screenshot);
        let Some(page) = self.shared.page().await else {
            // Браузера ещё нет — но человек смотрит именно потому, что бот не
            // вошёл. Пустое место в интерфейсе объяснило бы ему меньше, чем
            // пустая картинка.
            return Ok(PLACEHOLDER_PNG.to_vec());
        };
        match page.screenshot(&ScreenshotOptions::default()).await {
            Ok(png) => Ok(png),
            Err(e) => {
                tracing::warn!(target: "forge_meet", error = %e, "forge-meet: скриншот не снялся");
                Ok(PLACEHOLDER_PNG.to_vec())
            }
        }
    }

    async fn stop(&self, reason: StopReason) -> Result<(), MeetError> {
        if self.shared.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        *self.shared.requested.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
        self.shared.note(BotAction::Stop(reason));
        // Ждущий человека вход обязан проснуться и на остановку тоже, иначе
        // отменённая ассистированная встреча висела бы до перезапуска.
        self.shared.cancel.cancel();
        self.shared.human_notify.notify_waiters();

        // Даём фоновой задаче выйти самой: она дожмёт «покинуть встречу» и
        // погасит браузер штатно. Не дождались — гасим сами. ЖИВОГО CHROME
        // ПОСЛЕ ЭТОГО ВЫЗОВА НЕ ОСТАЁТСЯ НИ ПРИ КАКОМ ИСХОДЕ.
        let grace = Duration::from_millis(self.cfg.timings.terminate_grace_ms);
        let deadline = Instant::now() + grace;
        while !self.shared.finished.load(Ordering::SeqCst) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.shared.terminate(grace).await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Фоновая задача
// ---------------------------------------------------------------------------

async fn run(
    cfg: Arc<ChromeBotConfig>,
    shared: Arc<Shared>,
    mode: JoinMode,
    events: mpsc::UnboundedSender<MeetEvent>,
    audio: mpsc::Sender<AudioFrame>,
) {
    let started = Instant::now();
    // Аренда живёт ДОЛЬШЕ `drive`: Chrome гасится уже после его возврата, и
    // отдай мы профиль раньше — следующая встреча схватила бы каталог, из
    // которого ещё пишет умирающий браузер.
    let lease = ProfileLease::acquire(cfg.persist_profile);
    match drive(&cfg, &shared, mode, &events, &audio, started, &lease).await {
        Ok(reason) => {
            let at_ms = shared
                .axis
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .at_ms(0.0)
                .unwrap_or(0)
                .max(elapsed_ms(started));
            let _ = events.send(MeetEvent::RecordingStopped { reason, at_ms });
        }
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                url = %cfg.url,
                error = %e,
                "forge-meet: встреча не состоялась"
            );
            let _ = events.send(MeetEvent::Failed {
                code: e.code().to_string(),
                message: e.message(),
            });
        }
    }
    shared
        .terminate(Duration::from_millis(cfg.timings.terminate_grace_ms))
        .await;
    drop(lease);
    shared.finished.store(true, Ordering::SeqCst);
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Полный ход встречи. Возвращает причину остановки либо ошибку запуска.
async fn drive(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    mode: JoinMode,
    events: &mpsc::UnboundedSender<MeetEvent>,
    audio: &mpsc::Sender<AudioFrame>,
    started: Instant,
    lease: &ProfileLease,
) -> Result<StopReason, MeetError> {
    // 1. Браузер. Отсутствие Chrome — это код лестницы ошибок, а не паника.
    //
    //    Постоянный профиль создаётся ЗАРАНЕЕ и с правами `0700`: положись мы
    //    на Chrome, каталог с живой сессией аккаунта появился бы с правами по
    //    умолчанию, то есть читаемым для всех.
    let launch_cfg = cfg.launch_config(lease.granted());
    if lease.granted() {
        if let Err(e) = ensure_private_dir(&launch_cfg.user_data_dir) {
            tracing::warn!(
                target: "forge_meet",
                path = %launch_cfg.user_data_dir.display(),
                error = %e,
                "forge-meet: каталог постоянного профиля не подготовился"
            );
        }
    }
    let chrome = launch(&launch_cfg).await.map_err(|e| {
        tracing::warn!(target: "forge_meet", error = %e, "forge-meet: Chrome не запустился");
        MeetError::DepUnavailable("chrome_unavailable")
    })?;
    let ws_url = chrome.ws_url.clone();
    *shared.chrome.lock().unwrap_or_else(|e| e.into_inner()) = Some(chrome);

    // 2. Соединение и вкладка.
    let transport = WsTransport::connect(
        &ws_url,
        TransportConfig {
            command_timeout: Duration::from_millis(cfg.cdp_command_timeout_ms.max(1_000)),
            ..TransportConfig::default()
        },
    )
    .await
    .map_err(|e| MeetError::Internal(format!("нет соединения с CDP: {e}")))?;
    let browser = CdpBrowser::new(transport);
    let mut page = CdpPage::open(&browser)
        .await
        .map_err(|e| MeetError::Internal(format!("вкладка не открылась: {e}")))?;

    // 3. ПОДПИСКА ДО НАВИГАЦИИ. Первые кадры приходят сразу после загрузки, и
    //    окно между `navigate` и `subscribe` — это потерянное начало записи.
    let setup = cfg.page_setup(mode);
    let rx = forge_cdp::BindingReceiver::new(
        page.session().subscribe(),
        setup.inject.binding_name.clone(),
    );

    page.prepare_and_navigate(&cfg.url, &setup)
        .await
        .map_err(|e| MeetError::Internal(format!("навигация не удалась: {e}")))?;
    let page = Arc::new(page);
    *shared.page.write().await = Some(page.clone());

    // 4. Насос звука и событий. Он крутится ВСЮ встречу, включая время, пока
    //    человек проходит капчу: инжект уже стоит, и звук пойдёт с первой
    //    дорожки.
    let pump = tokio::spawn(pump_binding(
        shared.clone(),
        CdpBindingTransport::new(rx, cfg.sample_rate),
        events.clone(),
        audio.clone(),
    ));

    // 5. Вход.
    let outcome = match mode {
        JoinMode::Assisted => {
            // НИ ОДНОГО КЛИКА. Окно уже показано (`page_setup`), дальше
            // работает человек.
            shared.note(BotAction::ShowWindow(true));
            JoinOutcome::NeedsHuman("assisted".to_string())
        }
        JoinMode::Normal => join_flow(cfg, shared, &page, events, started).await?,
    };
    match outcome {
        JoinOutcome::Inside => {}
        JoinOutcome::Ended => {
            // Комната закрыта. Ни `needs_human`, ни ожидания: единственный
            // честный ответ — обычный конец встречи. Запись не начиналась, и
            // общий разбор исхода (в том числе решение «провал или нет»)
            // остаётся там же, где для всех прочих причин остановки.
            pump.abort();
            return Ok(requested_or(shared, StopReason::DomEnded));
        }
        JoinOutcome::NeedsHuman(kind) => {
            if kind != "assisted" {
                let _ = events.send(MeetEvent::NeedsHuman { kind: kind.clone() });
            }
            if !wait_for_human(shared).await {
                // Отмена вместо подтверждения.
                pump.abort();
                return Ok(requested_or(shared, StopReason::Shutdown));
            }
        }
    }

    shared.emit_joined_once(events);

    // 6. ЗВУК ВСТРЕЧИ. У веб-клиента Zoom без этого клика в странице нет ни
    //    одного источника звука — тапы сидели бы на пустом графе. У остальных
    //    площадок проба пуста и шага не существует.
    ensure_audio_joined(cfg, shared, &page).await;

    // 7. МИКРОФОН ГАСИТСЯ ПОВТОРНО ПОСЛЕ ВХОДА. Ф0: Телемост включает его сам
    //    в момент подключения, и pre-join-состояние ничего не гарантирует.
    apply_media(cfg, shared, &page, false, false).await;

    // 8. Наблюдение до конца встречи.
    let reason = monitor(cfg, shared, &page, events, started).await;

    // 9. Выход: сначала кнопкой (площадка увидит корректный уход), потом
    //    браузером. Пауза даёт worklet'у дофлашить последние кадры.
    leave(cfg, shared, &page).await;
    tokio::time::sleep(Duration::from_millis(cfg.timings.leave_grace_ms)).await;
    pump.abort();
    Ok(reason)
}

fn requested_or(shared: &Arc<Shared>, fallback: StopReason) -> StopReason {
    shared
        .requested
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or(fallback)
}

/// Ждёт человека. `false` — вместо подтверждения пришла отмена.
async fn wait_for_human(shared: &Arc<Shared>) -> bool {
    loop {
        if shared.human_ready.load(Ordering::SeqCst) {
            return true;
        }
        if shared.cancel.is_cancelled() {
            return false;
        }
        let notified = shared.human_notify.notified();
        tokio::select! {
            _ = notified => {}
            _ = shared.cancel.cancelled() => return shared.human_ready.load(Ordering::SeqCst),
        }
    }
}

/// Насос: сообщения страницы → события бота и кадры звука.
///
/// Отдельная задача, а не ветка `select!`, ровно по той же причине, по какой
/// [`super::scripted::ScriptedBot`] проигрывает сценарий задачей: канал звука
/// ОГРАНИЧЕН, и backpressure обязан тормозить приём из CDP, а не наблюдение за
/// встречей.
async fn pump_binding(
    shared: Arc<Shared>,
    mut transport: CdpBindingTransport,
    events: mpsc::UnboundedSender<MeetEvent>,
    audio: mpsc::Sender<AudioFrame>,
) {
    while let Some(msg) = transport.recv().await {
        match msg {
            PageMessage::Audio(frame) => {
                if audio.send(frame).await.is_err() {
                    return; // рекордер закрылся
                }
            }
            PageMessage::Meta { dropped, .. } => {
                if dropped > 0 {
                    let _ = events.send(MeetEvent::FramesDropped { total: dropped });
                }
            }
            PageMessage::Event(ev) => {
                if let Some(meet) = page_event_to_meet(&shared, &ev) {
                    // Со страницы пришло событие захвата — значит бот уже
                    // внутри, что бы ни думал цикл входа.
                    shared.emit_joined_once(&events);
                    if events.send(meet).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// Перевод сырого события страницы в событие бота.
///
/// Не все события переводятся: `tap_error`, `taps_installed` и
/// `tap_suppressed` — диагностика захвата, у неё нет места в контракте
/// порядка. Они уходят в лог.
fn page_event_to_meet(shared: &Arc<Shared>, ev: &super::transport::PageEvent) -> Option<MeetEvent> {
    let track = |ev: &super::transport::PageEvent| ev.str_field("t").unwrap_or_default().to_string();
    match ev.name.as_str() {
        "recording_started" => {
            let unix = ev.u64_field("unix").unwrap_or(0) as i64;
            shared.t0_unix.store(unix, Ordering::SeqCst);
            Some(MeetEvent::RecordingStarted { t0_unix_ms: unix })
        }
        "track_added" => {
            shared.tracks_seen.fetch_add(1, Ordering::SeqCst);
            shared.tracks_live.fetch_add(1, Ordering::SeqCst);
            Some(MeetEvent::TrackAdded {
                track_id: track(ev),
                idx: ev.u64_field("idx").unwrap_or(0) as usize,
                start_offset_ms: ev.u64_field("off").unwrap_or(ev.at_ms),
                // `m` — `msid` потока, в котором дорожка приехала. Его может
                // не быть вовсе (площадка связь не раскрывает), и это норма.
                msid: ev
                    .str_field("m")
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty()),
            })
        }
        // УТОЧНЕНИЕ `msid` ПОСЛЕ РЕНЕГОТИАЦИИ. В контракт событий не входит:
        // разбор журнала заводит дорожку по первому упоминанию, и связь,
        // приехавшая позже, к ней уже не привяжется. В лог — входит: без него
        // расхождение «в журнале один msid, в SDP другой» неразличимо.
        "track_msid" => {
            tracing::debug!(
                target: "forge_meet",
                track = %track(ev),
                msid = %ev.str_field("m").unwrap_or_default(),
                was = %ev.str_field("was").unwrap_or_default(),
                "forge-meet: msid дорожки уточнён ренеготиацией"
            );
            None
        }
        "track_ended" => {
            // `fetch_update`, а не `fetch_sub`: повторное событие о той же
            // дорожке иначе увело бы счётчик в максимум `usize` и «все
            // дорожки кончились» не сработало бы никогда.
            let _ = shared
                .tracks_live
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    Some(v.saturating_sub(1))
                });
            Some(MeetEvent::TrackEnded {
                track_id: track(ev),
                idx: ev.u64_field("idx").unwrap_or(0) as usize,
                end_offset_ms: ev.u64_field("at").unwrap_or(ev.at_ms),
            })
        }
        "speech_start" => {
            shared.speaking_tracks.fetch_add(1, Ordering::SeqCst);
            shared.speech_seen.store(true, Ordering::SeqCst);
            Some(MeetEvent::SpeechStarted {
                track_id: track(ev),
                at_ms: ev.u64_field("at").unwrap_or(ev.at_ms),
            })
        }
        "speech_end" => {
            let _ = shared
                .speaking_tracks
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    Some(v.saturating_sub(1))
                });
            Some(MeetEvent::SpeechEnded {
                track_id: track(ev),
                at_ms: ev.u64_field("at").unwrap_or(ev.at_ms),
            })
        }
        "capture_path" => Some(MeetEvent::CapturePathDetected {
            path: ev.str_field("p").unwrap_or("unknown").to_string(),
        }),
        "tap_error" | "transport_error" => {
            tracing::warn!(
                target: "forge_meet",
                event = %ev.name,
                fields = %ev.fields,
                "forge-meet: страница сообщила об ошибке тапа"
            );
            None
        }
        other => {
            tracing::debug!(target: "forge_meet", event = %other, "forge-meet: событие страницы");
            None
        }
    }
}

/// Снимок страницы плюс готовое наблюдение.
async fn snapshot(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
) -> Result<(PageSnapshot, Observation), MeetError> {
    let expr = observe_expression(&cfg.probes);
    let res = page
        .session()
        .call(
            "Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true}),
        )
        .await
        .map_err(|e| MeetError::Internal(format!("наблюдение не выполнилось: {e}")))?;
    let raw = res["result"]["value"]
        .as_str()
        .ok_or_else(|| MeetError::Internal("наблюдение вернуло не строку".to_string()))?;
    let snap = PageSnapshot::parse(raw)
        .map_err(|e| MeetError::Internal(format!("снимок не разобрался: {e}")))?;

    if !snap.participants.is_empty() {
        shared.participants_known.store(true, Ordering::SeqCst);
    }

    let t0 = shared.t0_unix.load(Ordering::SeqCst);
    let at_ms = {
        let mut axis = shared.axis.lock().unwrap_or_else(|e| e.into_inner());
        if t0 > 0 {
            axis.anchor(&snap, t0);
        }
        axis.at_ms(snap.perf)
    };
    let participants = shared
        .namer
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .label(&snap.participants);

    let obs = Observation {
        at_ms: at_ms.unwrap_or(0),
        active_speaker: snap.active.clone(),
        tracks: TrackObservation {
            audio_tracks: shared.tracks_live.load(Ordering::SeqCst),
            participants: snap.participants.len(),
            mid_stable: true,
            multi_csrc: false,
            since_join_ms: at_ms.unwrap_or(0),
        },
        participants,
    };
    *shared.last.write().await = obs.clone();
    Ok((snap, obs))
}

/// Ведёт встречу до конца и возвращает причину остановки.
async fn monitor(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
    events: &mpsc::UnboundedSender<MeetEvent>,
    started: Instant,
) -> StopReason {
    let mut watch = EndWatch::new(cfg.end);
    let mut failures = 0u32;
    let mut last_names: Vec<String> = Vec::new();
    let mut last_active: Option<String> = None;
    let mut last_mode = super::TrackMode::Unknown;
    let interval = Duration::from_millis(cfg.timings.observe_interval_ms.max(50));
    // Два стража «бот не слышит встречу»: один смотрит на расхождение DOM и
    // VAD, другой дочитывает `chrome.log`. Оба одноразовые по причине.
    let mut deaf = super::medialog::DeafWatch::new();
    let mut media_log = super::medialog::MediaLogWatch::new(&cfg.meeting_dir);

    loop {
        tokio::select! {
            _ = shared.cancel.cancelled() => {
                return requested_or(shared, StopReason::Shutdown);
            }
            _ = tokio::time::sleep(interval) => {}
        }

        let (snap, obs) = match snapshot(cfg, shared, page).await {
            Ok(v) => {
                failures = 0;
                v
            }
            Err(e) => {
                // Одиночный промах ничего не значит: страница могла как раз
                // перерисовываться. Несколько подряд означают, что target
                // умер — вкладку закрыли, Chrome упал.
                failures += 1;
                tracing::debug!(target: "forge_meet", error = %e, failures, "forge-meet: наблюдение не удалось");
                if failures >= cfg.timings.observe_failures_to_give_up {
                    return StopReason::BrowserGone;
                }
                continue;
            }
        };

        // События наблюдения имеют метку на общей оси, а её не существует до
        // `recording_started` — контракт порядка это прямо запрещает.
        let axis_ready = shared
            .axis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_set();
        if axis_ready {
            let names: Vec<String> = obs.participants.iter().map(|p| p.name.clone()).collect();
            if names != last_names {
                last_names = names;
                let _ = events.send(MeetEvent::ParticipantsChanged {
                    at_ms: obs.at_ms,
                    participants: obs.participants.clone(),
                });
            }
            if snap.active != last_active {
                last_active = snap.active.clone();
                let _ = events.send(MeetEvent::ActiveSpeaker {
                    at_ms: obs.at_ms,
                    name: snap.active.clone(),
                });
            }
            let mode = super::detect_track_mode(&obs.tracks, super::mode::DEFAULT_MODE_WARMUP_MS);
            if mode != super::TrackMode::Unknown && mode != last_mode {
                last_mode = mode;
                let _ = events.send(MeetEvent::TrackModeDetected {
                    at_ms: obs.at_ms,
                    mode,
                });
            }
        }

        // ГЛУХОТА БОТА. Проверяется только когда ось уже есть: до
        // `recording_started` звука нет по построению, и молчание VAD в это
        // время ничего не значит.
        if axis_ready {
            let dom_speaking = snap.speaking || snap.active.is_some();
            let vad_fired = shared.speech_seen.load(Ordering::SeqCst);
            if let Some(detail) = deaf.observe(dom_speaking, vad_fired, interval.as_millis() as u64)
            {
                let _ = events.send(MeetEvent::MediaWarning {
                    at_ms: obs.at_ms,
                    code: super::medialog::CODE_DEAF_BOT.to_string(),
                    detail,
                });
            }
            // Тот же вход, что у стража глухоты, и это не дублирование: строка
            // отказа в логе сама по себе не означает пустой записи (штатный
            // перебор ICE-кандидатов печатает такие же), поэтому причина
            // называется только вместе с подтверждённой глухотой.
            for detail in media_log.poll(dom_speaking, vad_fired, interval.as_millis() as u64) {
                let _ = events.send(MeetEvent::MediaWarning {
                    at_ms: obs.at_ms,
                    code: super::medialog::CODE_MEDIA_TRANSPORT.to_string(),
                    detail,
                });
            }
        }

        let signals = EndSignals {
            target_gone: false,
            tracks_seen: shared.tracks_seen.load(Ordering::SeqCst),
            tracks_live: shared.tracks_live.load(Ordering::SeqCst),
            dom_ended: snap.ended,
            post_call: snap.post_call,
            participants_known: shared.participants_known.load(Ordering::SeqCst),
            others: snap.others(&cfg.bot_name),
            // Речь берётся из ДВУХ источников: пер-трековый VAD (работает без
            // всяких проб) и DOM-индикатор. Один из них слепнет вместе с
            // вёрсткой, второй — нет.
            speaking: shared.speaking_tracks.load(Ordering::SeqCst) > 0 || snap.speaking,
        };
        if let Some(reason) = watch.observe(elapsed_ms(started), signals) {
            tracing::info!(
                target: "forge_meet",
                url = %cfg.url,
                reason = reason.as_str(),
                "forge-meet: встреча закончилась"
            );
            return reason;
        }
    }
}

/// Приводит микрофон и камеру к нужному состоянию.
///
/// Не возвращает ошибку НИ ПРИ КАКОМ ИСХОДЕ. Кнопки может не быть (пробы
/// протухли, площадка сменила вёрстку), и это повод для `warn`, а не для
/// отмены записи: фейковое устройство с файлом тишины остаётся первым и
/// главным барьером.
async fn apply_media(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
    mic: bool,
    cam: bool,
) {
    ensure_device(
        cfg,
        shared,
        page,
        mic,
        probe::MIC_MUTE_BUTTON,
        probe::MIC_UNMUTE_BUTTON,
        probe::MIC_TOGGLE_HOTKEY,
        "микрофон",
        |s| s.mic(),
    )
    .await;
    ensure_device(
        cfg,
        shared,
        page,
        cam,
        probe::CAM_DISABLE_BUTTON,
        probe::CAM_ENABLE_BUTTON,
        probe::CAM_TOGGLE_HOTKEY,
        "камера",
        |s| s.cam(),
    )
    .await;
}

/// Жмёт «подключить звук компьютера», если у площадки такая кнопка есть.
///
/// # Почему это отдельный шаг, а не ещё один клик входа
///
/// Потому что у веб-клиента Zoom БЕЗ ЭТОГО КЛИКА В СТРАНИЦЕ НЕТ ЗВУКА ВООБЩЕ:
/// не тише и не хуже, а ни одного источника — тапы захвата сидели бы на пустом
/// графе, и запись вышла бы нулевой длины при полностью исправном боте.
/// Кнопка появляется УЖЕ ВНУТРИ встречи, то есть после того, как цикл входа
/// закончился, и в `next_join_step` ей места нет.
///
/// Best-effort по той же причине, что и всё остальное здесь: у Телемоста, Meet
/// и Teams проба пуста, шаг просто не существует, и отсутствие кнопки не
/// является ошибкой ни для одной площадки.
async fn ensure_audio_joined(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
) {
    if cfg.probes.get(probe::JOIN_AUDIO_BUTTON).is_empty() {
        return;
    }
    for attempt in 0..cfg.timings.join_audio_attempts {
        let visible = match snapshot(cfg, shared, page).await {
            Ok((snap, _)) => snap.ui.join_audio,
            Err(_) => false,
        };
        if !visible {
            // Кнопки нет: либо звук уже подключён, либо площадка её не
            // показывает. Оба случая — не ошибка.
            if attempt > 0 {
                tracing::info!(target: "forge_meet", attempt, "forge-meet: звук встречи подключён");
            }
            return;
        }
        click_probe(cfg, shared, page, probe::JOIN_AUDIO_BUTTON).await;
    }
    tracing::warn!(
        target: "forge_meet",
        "forge-meet: кнопка подключения звука не исчезла — записываю как есть"
    );
}

/// Жмёт кнопку устройства, пока состояние не станет нужным.
///
/// До ТРЁХ попыток: интерфейс встречи перерисовывается не мгновенно, и первый
/// клик иногда уходит в ещё не смонтированную кнопку. Молча смириться нельзя —
/// цена ошибки в сторону «включено» это бот, транслирующий в чужую встречу.
///
/// # Два пути действия, и порядок между ними не случаен
///
/// 1. **Сочетание клавиш** — первым, если оно задано пробой платформы. У
///    Google Meet это `Ctrl+D` и `Ctrl+E`: они не локализуются и не менялись
///    годами, в отличие от `aria-label`, который переводится на язык
///    интерфейса и переписывается вместе с редизайном.
/// 2. **Клик по кнопке** — всегда, начиная со второй попытки, и единственным
///    путём там, где сочетания нет (Телемост).
///
/// Сочетание при этом НИКОГДА не шлётся вслепую: см. [`hotkey_allowed`].
/// Состояние устройства читается пробами кнопок в обоих случаях — «выключить»
/// и «включить» это РАЗНЫЕ пробы, и отсутствие обеих означает «не знаю».
async fn ensure_device(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
    want_on: bool,
    turn_off_probe: &str,
    turn_on_probe: &str,
    hotkey_probe: &str,
    what: &str,
    read: impl Fn(&PageSnapshot) -> DeviceState,
) {
    let target_probe = if want_on { turn_on_probe } else { turn_off_probe };
    let hotkey = cfg.probes.first(hotkey_probe).and_then(Hotkey::parse);
    if cfg.probes.get(target_probe).is_empty() && hotkey.is_none() {
        tracing::warn!(
            target: "forge_meet",
            what,
            probe = target_probe,
            "forge-meet: пробы кнопки нет — состояние устройства остаётся на совести флагов запуска"
        );
        return;
    }

    for attempt in 0..3u32 {
        let state = match snapshot(cfg, shared, page).await {
            Ok((snap, _)) => read(&snap),
            Err(_) => DeviceState::Unknown,
        };
        let done = match (want_on, state) {
            (true, DeviceState::On) | (false, DeviceState::Off) => true,
            // «Не знаю» никогда не считается достигнутым состоянием: кнопку
            // жмём, если она нашлась.
            _ => false,
        };
        if done {
            if attempt > 0 {
                tracing::info!(target: "forge_meet", what, attempt, "forge-meet: устройство приведено в нужное состояние");
            }
            return;
        }

        // Первая попытка — сочетанием клавиш, если оно есть И состояние
        // известно. Дальше кнопкой: сочетание уже показало, что не сработало.
        if attempt == 0 {
            if let Some(hk) = hotkey.as_ref().filter(|_| hotkey_allowed(want_on, state)) {
                match press_hotkey(page, hk).await {
                    Ok(()) => {
                        shared.note(BotAction::Click(hotkey_probe.to_string()));
                        tracing::debug!(target: "forge_meet", what, key = %hk.key, "forge-meet: сочетание клавиш отправлено");
                        tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
                        continue;
                    }
                    Err(e) => tracing::debug!(
                        target: "forge_meet", what, error = %e,
                        "forge-meet: сочетание клавиш не отправилось, иду кнопкой"
                    ),
                }
            }
        }

        if cfg.probes.get(target_probe).is_empty() {
            tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            continue;
        }
        let expr = probe_element_expression(&cfg.probes, target_probe);
        match page.click_expression(&expr).await {
            Ok(_) => {
                shared.note(BotAction::Click(target_probe.to_string()));
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            }
            Err(e) => {
                tracing::debug!(target: "forge_meet", what, error = %e, "forge-meet: кнопка устройства не нажалась");
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            }
        }
    }
    tracing::warn!(
        target: "forge_meet",
        what,
        want_on,
        "forge-meet: состояние устройства подтвердить не удалось — запись продолжается"
    );
}

/// Отправляет сочетание клавиш странице.
///
/// `rawKeyDown` + `keyUp`, а не `char`: площадки слушают именно `keydown` с
/// модификаторами. Событие идёт через `Input.dispatchKeyEvent`, то есть
/// приходит с `isTrusted: true` — синтетический `KeyboardEvent` из
/// `Runtime.evaluate` Teams и Zoom просто проигнорировали бы.
async fn press_hotkey(page: &Arc<CdpPage>, hk: &Hotkey) -> Result<(), MeetError> {
    let key = hk.key.to_string();
    let code = hk.code();
    let vk = hk.vk();
    for kind in ["rawKeyDown", "keyUp"] {
        page.session()
            .call(
                "Input.dispatchKeyEvent",
                serde_json::json!({
                    "type": kind,
                    "modifiers": hk.modifiers,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": vk,
                    "nativeVirtualKeyCode": vk,
                }),
            )
            .await
            .map_err(|e| MeetError::Internal(format!("клавиша не отправилась: {e}")))?;
    }
    Ok(())
}

/// Проходит вход по пробам.
async fn join_flow(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
    events: &mpsc::UnboundedSender<MeetEvent>,
    started: Instant,
) -> Result<JoinOutcome, MeetError> {
    let deadline = Instant::now() + Duration::from_millis(cfg.timings.join_timeout_ms);
    let mut progress = JoinProgress::default();
    let mut lobby_seen = false;
    let mut guest_branch: Option<bool> = None;

    while Instant::now() < deadline {
        if shared.cancel.is_cancelled() {
            return Ok(JoinOutcome::NeedsHuman("cancelled".to_string()));
        }
        let snap = match snapshot(cfg, shared, page).await {
            Ok((snap, _)) => snap,
            Err(e) => {
                tracing::debug!(target: "forge_meet", error = %e, "forge-meet: снимок при входе не удался");
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
                continue;
            }
        };

        match next_join_step(&snap, &progress, cfg.timings.join_step_attempts) {
            JoinStep::Captcha => {
                // Капчу мы не решаем автоматически НИ ПРИ КАКИХ УСЛОВИЯХ.
                return Ok(JoinOutcome::NeedsHuman("captcha".to_string()));
            }
            JoinStep::Denied => {
                // ПРОДУКТОВОЕ ОГРАНИЧЕНИЕ, А НЕ НАША ПОЛОМКА. Ждать нечего:
                // Meet отклоняет анонимную заявку сразу и повторные попытки
                // ничего не изменят. Человеку нужен честный текст, а не
                // молчаливая минута до таймаута.
                tracing::warn!(
                    target: "forge_meet",
                    url = %cfg.url,
                    elapsed_ms = elapsed_ms(started),
                    "forge-meet: площадка отказала во входе"
                );
                return Ok(JoinOutcome::NeedsHuman("denied".to_string()));
            }
            JoinStep::Ended => {
                // ВСТРЕЧА МОЖЕТ КОНЧИТЬСЯ, НЕ НАЧАВШИСЬ. Мёртвая комната
                // vcc.airlabs.pro отдаёт «Встреча завершена» ещё до
                // пре-джойна: ни поля имени, ни кнопки входа на странице нет
                // вовсе. Без этой ветки бот стоял бы до таймаута, а человек
                // получил бы `needs_human` с причиной `unknown` — то есть
                // закрытая ссылка выглядела бы поломкой бота.
                tracing::info!(
                    target: "forge_meet",
                    url = %cfg.url,
                    elapsed_ms = elapsed_ms(started),
                    "forge-meet: встреча уже завершена — записывать нечего"
                );
                return Ok(JoinOutcome::Ended);
            }
            JoinStep::Inside => {
                tracing::info!(
                    target: "forge_meet",
                    guest = ?guest_branch,
                    elapsed_ms = elapsed_ms(started),
                    "forge-meet: бот внутри встречи"
                );
                return Ok(JoinOutcome::Inside);
            }
            JoinStep::ContinueInBrowser => {
                progress.interstitial_attempts += 1;
                click_probe(cfg, shared, page, probe::CONTINUE_IN_BROWSER).await;
            }
            JoinStep::TypeName => {
                // ГОСТЕВАЯ ВЕТКА. Поле есть — значит профиль не залогинен.
                guest_branch = Some(true);
                let expr = probe_element_expression(&cfg.probes, probe::NAME_INPUT);
                match focus_and_type(page, &expr, &cfg.bot_name).await {
                    Ok(()) => {
                        shared.note(BotAction::Type(cfg.bot_name.clone()));
                        progress.name_typed = true;
                    }
                    Err(e) => {
                        tracing::debug!(target: "forge_meet", error = %e, "forge-meet: имя не ввелось");
                        // Иначе шаг повторялся бы вечно: поле есть, ввод не
                        // удаётся, до кнопки входа очередь не доходит.
                        progress.name_typed = true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            }
            JoinStep::MuteDevices => {
                // Один раз за вход, чем бы дело ни кончилось: `apply_media`
                // сам делает до трёх попыток на устройство и никогда не
                // возвращает ошибки. Повторять его в цикле входа означало бы
                // отдать ему весь таймаут.
                progress.media_muted = true;
                apply_media(cfg, shared, page, false, false).await;
            }
            JoinStep::Join => {
                if guest_branch.is_none() {
                    // ВЕТКА ЗАЛОГИНЕННОГО ПРОФИЛЯ: поля имени нет вовсе, бот
                    // войдёт под именем аккаунта. Ф0 измерила это на живом
                    // Телемосте — и это не ошибка, а второй законный путь.
                    guest_branch = Some(false);
                    tracing::info!(
                        target: "forge_meet",
                        "forge-meet: поля имени нет — вхожу веткой залогиненного профиля"
                    );
                }
                progress.join_attempts += 1;
                click_probe(cfg, shared, page, probe::JOIN_BUTTON).await;
            }
            JoinStep::Lobby => {
                if !lobby_seen {
                    lobby_seen = true;
                    // Статус `waiting_admit`, а не `needs_human`: человек тут
                    // ничего сделать не может, решение за организатором.
                    let _ = events.send(MeetEvent::WaitingAdmit);
                    tracing::info!(target: "forge_meet", "forge-meet: лобби, жду допуска");
                }
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            }
            JoinStep::Wait => {
                tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
            }
        }
    }

    // Не уложились. Это НЕ провал: дальше подхватывает ассистированный вход —
    // человек видит скриншот, доводит вход руками и жмёт «я внутри».
    tracing::warn!(
        target: "forge_meet",
        url = %cfg.url,
        lobby_seen,
        interstitial_attempts = progress.interstitial_attempts,
        join_attempts = progress.join_attempts,
        "forge-meet: вход не удался за отведённое время, зову человека"
    );
    Ok(JoinOutcome::NeedsHuman(
        if lobby_seen { "lobby" } else { "unknown" }.to_string(),
    ))
}

/// Кликает по элементу пробы. Промах пробы — не ошибка встречи.
async fn click_probe(
    cfg: &Arc<ChromeBotConfig>,
    shared: &Arc<Shared>,
    page: &Arc<CdpPage>,
    name: &str,
) {
    let expr = probe_element_expression(&cfg.probes, name);
    match page.click_expression(&expr).await {
        Ok(center) => {
            shared.note(BotAction::Click(name.to_string()));
            tracing::debug!(
                target: "forge_meet",
                probe = name,
                x = center.x,
                y = center.y,
                "forge-meet: клик по пробе"
            );
        }
        Err(e) => tracing::debug!(target: "forge_meet", probe = name, error = %e, "forge-meet: проба не нажалась"),
    }
    tokio::time::sleep(Duration::from_millis(cfg.timings.step_settle_ms)).await;
}

/// Наводит фокус на найденный пробой элемент и печатает текст.
///
/// Только `Input.insertText`, никогда `el.value = x`: React хранит последнее
/// известное значение в трекере на самом узле и на прямое присваивание не
/// реагирует вовсе.
async fn focus_and_type(page: &Arc<CdpPage>, element_expr: &str, text: &str) -> Result<(), MeetError> {
    let expression = format!(
        "(function () {{ var el = ({element_expr}); if (!el) return \"missing\"; \
         if (el.focus) el.focus(); if (el.select) el.select(); return \"ok\"; }})()"
    );
    let res = page
        .session()
        .call(
            "Runtime.evaluate",
            serde_json::json!({"expression": expression, "returnByValue": true}),
        )
        .await
        .map_err(|e| MeetError::Internal(format!("фокус не получен: {e}")))?;
    if res["result"]["value"].as_str() != Some("ok") {
        return Err(MeetError::Internal("поле имени не найдено".to_string()));
    }
    page.session()
        .call("Input.insertText", serde_json::json!({ "text": text }))
        .await
        .map_err(|e| MeetError::Internal(format!("текст не ввёлся: {e}")))?;
    Ok(())
}

/// Жмёт «покинуть встречу», если проба есть.
///
/// Best-effort: браузер всё равно будет погашен следом. Кнопка нужна не нам, а
/// участникам — иначе бот висит в списке ещё минуту после ухода.
async fn leave(cfg: &Arc<ChromeBotConfig>, shared: &Arc<Shared>, page: &Arc<CdpPage>) {
    if cfg.probes.get(probe::LEAVE_BUTTON).is_empty() {
        return;
    }
    let expr = probe_element_expression(&cfg.probes, probe::LEAVE_BUTTON);
    if page.click_expression(&expr).await.is_ok() {
        shared.note(BotAction::Click(probe::LEAVE_BUTTON.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Фабрика
// ---------------------------------------------------------------------------

/// Источник боевых ботов.
///
/// Ставится в [`crate::state::MeetState`] на старте плагина и заменяет
/// [`crate::recording::MissingBotFactory`], который до Ф4 отвечал
/// `chrome_unavailable` на любую попытку записи.
pub struct ChromeBotFactory {
    config: MeetConfig,
    probes: ProbeBook,
    timings: BotTimings,
}

impl ChromeBotFactory {
    pub fn new(config: MeetConfig) -> Self {
        Self {
            config,
            probes: ProbeBook::load(),
            timings: BotTimings::default(),
        }
    }

    /// Фабрика с явной книгой проб — для тестов и для мок-встречи.
    pub fn with_probes(config: MeetConfig, probes: ProbeBook) -> Self {
        Self {
            config,
            probes,
            timings: BotTimings::default(),
        }
    }

    /// Заменяет тайминги (тесты инжектят миллисекунды).
    pub fn with_timings(mut self, timings: BotTimings) -> Self {
        self.timings = timings;
        self
    }

    /// Книга проб фабрики.
    pub fn probes(&self) -> &ProbeBook {
        &self.probes
    }

    /// Ключ платформы по URL встречи.
    ///
    /// Незнакомый адрес — пустой набор проб, а не отказ: запись важнее имён.
    fn platform_key(url: &str) -> String {
        crate::providers::parse_meeting_url(url)
            .map(|link| link.platform.as_str().to_string())
            .unwrap_or_else(|| super::platforms::MOCK_PLATFORM.to_string())
    }

    /// Адрес, по которому бот открывает страницу.
    ///
    /// Совпадает с каноническим у всех платформ, кроме Zoom: там надо идти
    /// сразу в веб-клиент, минуя страницу «скачайте приложение». Неразбираемый
    /// адрес (мок-встреча, ручной URL) остаётся как есть.
    fn entry_url(url: &str) -> String {
        crate::providers::parse_meeting_url(url)
            .map(|link| crate::providers::bot_entry_url(&link))
            .unwrap_or_else(|| url.to_string())
    }
}

impl BotFactory for ChromeBotFactory {
    fn create(&self, req: &BotRequest) -> Result<Arc<dyn MeetBot>, MeetError> {
        let key = Self::platform_key(&req.url);
        let probes = self.probes.for_platform(&key);
        if probes.is_empty() {
            tracing::warn!(
                target: "forge_meet",
                platform = %key,
                "forge-meet: проб для платформы нет — участники будут «Участник N»"
            );
        }
        let mut cfg = ChromeBotConfig::new(&self.config, req, probes);
        cfg.timings = self.timings;
        cfg.url = Self::entry_url(&req.url);
        if cfg.url != req.url {
            tracing::info!(
                target: "forge_meet",
                canonical = %req.url,
                entry = %cfg.url,
                "forge-meet: вход идёт по адресу веб-клиента платформы"
            );
        }
        Ok(Arc::new(ChromeBot::new(cfg)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probes() -> ProbeSet {
        ProbeBook::builtin().for_platform(super::super::platforms::MOCK_PLATFORM)
    }

    fn snap(ui: super::super::observe::PageUi) -> PageSnapshot {
        PageSnapshot {
            ui,
            ..PageSnapshot::default()
        }
    }

    fn ui() -> super::super::observe::PageUi {
        super::super::observe::PageUi::default()
    }

    /// Шаг входа с боевым числом попыток.
    fn step(snap: &PageSnapshot, progress: &JoinProgress) -> JoinStep {
        next_join_step(snap, progress, BotTimings::default().join_step_attempts)
    }

    /// ГОСТЕВАЯ ВЕТКА: есть поле имени — сначала имя, потом вход.
    #[test]
    fn the_guest_branch_types_a_name_before_joining() {
        let screen = snap(super::super::observe::PageUi {
            continue_in_browser: true,
            name_input: true,
            join_button: true,
            ..ui()
        });
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::ContinueInBrowser);

        let after_interstitial = snap(super::super::observe::PageUi {
            name_input: true,
            join_button: true,
            ..ui()
        });
        assert_eq!(step(&after_interstitial, &JoinProgress::default()), JoinStep::TypeName);
        assert_eq!(
            step(
                &after_interstitial,
                &JoinProgress { name_typed: true, ..JoinProgress::default() }
            ),
            JoinStep::Join
        );
    }

    /// ВЕТКА ЗАЛОГИНЕННОГО ПРОФИЛЯ: поля имени нет вовсе (измерено Ф0), и это
    /// не мешает войти.
    #[test]
    fn the_logged_in_branch_joins_without_a_name_field() {
        let screen = snap(super::super::observe::PageUi {
            join_button: true,
            ..ui()
        });
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Join);
    }

    /// Внутри встречи бот больше ничего не жмёт — даже если кнопка входа
    /// осталась в разметке.
    #[test]
    fn inside_the_meeting_nothing_gets_clicked_again() {
        let screen = snap(super::super::observe::PageUi {
            join_button: true,
            name_input: true,
            leave_button: true,
            ..ui()
        });
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Inside);
    }

    /// Капча выигрывает у всего: молотить по кнопкам под капчей бессмысленно.
    #[test]
    fn a_captcha_beats_every_other_step() {
        let screen = snap(super::super::observe::PageUi {
            captcha: true,
            leave_button: true,
            join_button: true,
            ..ui()
        });
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Captcha);
    }

    /// Лобби — это ожидание, а не отказ.
    #[test]
    fn a_lobby_is_a_wait_not_a_failure() {
        let screen = snap(super::super::observe::PageUi { lobby: true, ..ui() });
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Lobby);
    }

    /// ОТКАЗ — это не ожидание: ждать нечего, зовём человека сразу.
    ///
    /// Разница с лобби не косметическая. Лобби кончается тем, что организатор
    /// впустит; отказ Google Meet анонимному участнику не кончается ничем, и
    /// минута молчания до таймаута выглядела бы нашей поломкой.
    #[test]
    fn a_denial_is_not_a_wait() {
        let screen = PageSnapshot {
            denied: true,
            ..snap(super::super::observe::PageUi {
                join_button: true,
                ..ui()
            })
        };
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Denied);
    }

    /// СТРАЖ: находясь ВНУТРИ встречи, бот не уходит по маркеру отказа.
    ///
    /// Маркер отказа живёт в том же документе, что и панель встречи, а его
    /// селектор — черновик, снятый не живой пробой. Поймай он что-нибудь во
    /// время звонка — и протухшая строка TOML выкидывала бы бота с ЖИВОЙ
    /// встречи, обрывая запись. Кнопка выхода видна — значит мы внутри, и
    /// никакой текст этого не отменяет.
    #[test]
    fn a_stale_denial_marker_never_evicts_a_bot_that_is_already_inside() {
        let screen = PageSnapshot {
            denied: true,
            ..snap(super::super::observe::PageUi {
                leave_button: true,
                ..ui()
            })
        };
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Inside);
    }

    /// МЁРТВАЯ КОМНАТА — это конец встречи, а не таймаут входа.
    ///
    /// Замер Ф13 на vcc.airlabs.pro: `/r/zzz-zzz-zzz` отдаёт страницу
    /// «Встреча завершена» немедленно — ни поля имени, ни кнопки входа, ни
    /// кнопки выхода на ней нет. Без отдельного шага такой экран неотличим от
    /// «ничего знакомого», и бот молчал бы минуту, а человек получил бы
    /// `needs_human` с причиной `unknown` там, где делать ему нечего.
    #[test]
    fn a_dead_room_is_an_ended_meeting_not_a_timeout() {
        let screen = PageSnapshot {
            ended: true,
            ..snap(ui())
        };
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Ended);
    }

    /// СТРАЖ, ПАРНЫЙ К ОТКАЗУ: протухший маркер конца не выкидывает бота с
    /// ЖИВОЙ встречи.
    ///
    /// `contains:Встреча завершена` ищет короткую фразу среди всех узлов
    /// документа, а панель встречи и постзвонковый экран у площадок живут в
    /// ОДНОМ документе. Совпади фраза во время звонка — и бот ушёл бы с
    /// середины записи. Кнопка выхода видна — значит мы внутри.
    #[test]
    fn a_stale_ended_marker_never_evicts_a_bot_that_is_already_inside() {
        let screen = PageSnapshot {
            ended: true,
            ..snap(super::super::observe::PageUi {
                leave_button: true,
                ..ui()
            })
        };
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Inside);
    }

    /// УСТРОЙСТВА ГАСЯТСЯ ДО КЛИКА «ВОЙТИ», А НЕ ПОСЛЕ.
    ///
    /// До Ф13 микрофон и камера гасились только после входа, и бот на
    /// несколько секунд въезжал в чужую встречу с живым микрофоном. На
    /// пре-джойне vcc.airlabs.pro состояние читается однозначно (`mic = on`,
    /// `cam = on`, измерено), значит платить этими секундами незачем.
    #[test]
    fn the_devices_are_muted_before_the_join_is_clicked() {
        let prejoin = PageSnapshot {
            mic_raw: "on".to_string(),
            cam_raw: "on".to_string(),
            ..snap(super::super::observe::PageUi {
                name_input: true,
                join_button: true,
                ..ui()
            })
        };
        // Сначала имя: пустое имя у площадки делает кнопку входа неактивной.
        assert_eq!(step(&prejoin, &JoinProgress::default()), JoinStep::TypeName);
        let named = JoinProgress {
            name_typed: true,
            ..JoinProgress::default()
        };
        assert_eq!(step(&prejoin, &named), JoinStep::MuteDevices);
        // Шаг берётся ровно один раз: `apply_media` внутри делает свои три
        // попытки, и повторять его циклом входа значило бы отдать ему таймаут.
        let muted = JoinProgress {
            media_muted: true,
            ..named
        };
        assert_eq!(step(&prejoin, &muted), JoinStep::Join);
    }

    /// «НЕ ЗНАЮ» — НЕ ПОВОД ЖАТЬ КНОПКУ.
    ///
    /// У Телемоста и Google Meet снимок пре-джойна про устройства молчит
    /// (`mic = unknown`), и лишний шаг там означал бы клик вслепую по
    /// селектору, который ничего не нашёл. Вход обязан идти как прежде.
    #[test]
    fn an_unknown_device_state_adds_no_step_to_the_join() {
        let screen = snap(super::super::observe::PageUi {
            join_button: true,
            ..ui()
        });
        assert_eq!(screen.mic(), DeviceState::Unknown);
        assert_eq!(step(&screen, &JoinProgress::default()), JoinStep::Join);

        // И уже выключенное устройство шага тоже не рождает.
        let off = PageSnapshot {
            mic_raw: "off".to_string(),
            cam_raw: "off".to_string(),
            ..screen
        };
        assert_eq!(step(&off, &JoinProgress::default()), JoinStep::Join);
    }

    /// Пустой экран — ждём, а не выдумываем клики.
    #[test]
    fn an_unknown_screen_means_waiting() {
        assert_eq!(step(&snap(ui()), &JoinProgress::default()), JoinStep::Wait);
    }

    /// ПОЛНАЯ ДЕГРАДАЦИЯ ПРОБ: без единой пробы вход не находит ничего и
    /// честно зовёт человека, а не притворяется вошедшим.
    #[test]
    fn without_probes_the_join_finds_nothing_and_asks_for_a_human() {
        let empty = ProbeSet::empty();
        assert!(empty.get(probe::JOIN_BUTTON).is_empty());
        // Снимок такой площадки всегда пуст — шаг всегда «ждать».
        assert_eq!(step(&snap(ui()), &JoinProgress::default()), JoinStep::Wait);
    }

    /// НЕИСЧЕЗАЮЩАЯ КНОПКА НЕ ЗАЦИКЛИВАЕТ ВХОД.
    ///
    /// Найдено живым прогоном: мок-встреча после клика оставляет
    /// «Продолжить в браузере» в разметке (просто `disabled`), и бот жал её по
    /// кругу все двадцать секунд, не добравшись ни до имени, ни до входа.
    /// Настоящие площадки ведут себя так же чаще, чем хотелось бы, поэтому
    /// лечится это не пробой, а счётчиком попыток.
    #[test]
    fn a_button_that_never_disappears_does_not_loop_the_join_forever() {
        let screen = snap(super::super::observe::PageUi {
            continue_in_browser: true,
            name_input: true,
            join_button: true,
            ..ui()
        });
        let mut progress = JoinProgress::default();
        for _ in 0..BotTimings::default().join_step_attempts {
            assert_eq!(step(&screen, &progress), JoinStep::ContinueInBrowser);
            progress.interstitial_attempts += 1;
        }
        assert_eq!(
            step(&screen, &progress),
            JoinStep::TypeName,
            "исчерпав попытки, бот обязан идти дальше, а не жать одно и то же"
        );
        progress.name_typed = true;
        for _ in 0..BotTimings::default().join_step_attempts {
            assert_eq!(step(&screen, &progress), JoinStep::Join);
            progress.join_attempts += 1;
        }
        assert_eq!(
            step(&screen, &progress),
            JoinStep::Wait,
            "исчерпав всё, бот ждёт и зовёт человека, а не молотит по кнопке"
        );
    }

    /// Режимы окна двух крейтов переводятся один в один.
    #[test]
    fn the_window_modes_translate_one_to_one() {
        use crate::config::WindowMode as Cfg;
        use forge_cdp::launch::WindowMode as Cdp;
        assert_eq!(cdp_window_mode(Cfg::Offscreen), Cdp::Offscreen);
        assert_eq!(cdp_window_mode(Cfg::Minimized), Cdp::Minimized);
        assert_eq!(cdp_window_mode(Cfg::Visible), Cdp::Visible);
        assert_eq!(cdp_window_mode(Cfg::Headless), Cdp::Headless);
    }

    /// Ассистированный вход показывает окно СРАЗУ, обычный — нет.
    #[test]
    fn assisted_mode_starts_with_a_visible_window() {
        let cfg = ChromeBotConfig {
            url: "https://telemost.yandex.ru/j/1".into(),
            meeting_dir: PathBuf::from("/tmp/нет"),
            cache_dir: PathBuf::from("/tmp/нет"),
            bot_name: "F.O.R.G.E.".into(),
            probes: probes(),
            inject: inject::inject_config(&MeetConfig::default()),
            sample_rate: 16_000,
            window_mode: crate::config::WindowMode::Minimized,
            chrome_path: None,
            ffmpeg_path: None,
            mute_audio: true,
            cdp_command_timeout_ms: 15_000,
            persist_profile: false,
            end: EndConfig::default(),
            timings: BotTimings::default(),
        };
        assert_eq!(
            cfg.page_setup(JoinMode::Assisted).window_mode,
            forge_cdp::launch::WindowMode::Visible
        );
        assert_eq!(
            cfg.page_setup(JoinMode::Normal).window_mode,
            forge_cdp::launch::WindowMode::Minimized,
            "обычный режим обязан идти в дефолтном режиме окна"
        );
    }

    /// Фабрика выбирает набор проб ПО ПЛАТФОРМЕ ссылки.
    #[test]
    fn the_factory_picks_probes_by_platform() {
        assert_eq!(
            ChromeBotFactory::platform_key("https://telemost.yandex.ru/j/60497752704486"),
            "telemost"
        );
        assert_eq!(
            ChromeBotFactory::platform_key("https://meet.google.com/abc-defg-hij"),
            "meet"
        );
        // Мок-встреча живёт на 127.0.0.1 и платформой не является — её набор
        // проб и есть тот, по которому идут e2e.
        assert_eq!(
            ChromeBotFactory::platform_key("http://127.0.0.1:1234/index.html?mode=slots"),
            super::super::platforms::MOCK_PLATFORM
        );
    }

    /// Бот идёт в ВЕБ-КЛИЕНТ Zoom, а не по канонической ссылке.
    ///
    /// По `zoom.us/j/<id>` открывается страница «скачайте приложение», и
    /// кнопка «присоединиться из браузера» там спрятана. Канонический адрес
    /// при этом остаётся прежним — он показывается человеку и держит
    /// идентичность комнаты.
    #[test]
    fn zoom_is_entered_through_the_web_client_and_keeps_its_password() {
        let entry = ChromeBotFactory::entry_url("https://us02web.zoom.us/j/87654321098?pwd=Qm9iSXM");
        assert!(entry.starts_with("https://app.zoom.us/wc/join/87654321098"), "{entry}");
        assert!(entry.contains("pwd=Qm9iSXM"), "пароль потерян: {entry}");

        // Остальные площадки не трогаются вовсе.
        let telemost = "https://telemost.yandex.ru/j/60497752704486";
        assert_eq!(ChromeBotFactory::entry_url(telemost), telemost);
        // Неразбираемый адрес (мок) остаётся как есть — иначе e2e ходил бы в
        // никуда.
        let mock = "http://127.0.0.1:1234/index.html?mode=slots";
        assert_eq!(ChromeBotFactory::entry_url(mock), mock);
    }

    /// Разбор сочетаний клавиш: что принимается и что отвергается.
    #[test]
    fn hotkeys_parse_modifiers_and_reject_garbage() {
        let ctrl_d = Hotkey::parse("ctrl+d").expect("ctrl+d обязан разбираться");
        assert_eq!(ctrl_d.modifiers, 2);
        assert_eq!(ctrl_d.key, 'd');
        assert_eq!(ctrl_d.code(), "KeyD");
        assert_eq!(ctrl_d.vk(), 'D' as u32);

        assert_eq!(Hotkey::parse("CTRL+Shift+M").unwrap().modifiers, 2 | 8);
        assert_eq!(Hotkey::parse("alt+a").unwrap().modifiers, 1);
        assert_eq!(Hotkey::parse("meta+shift+1").unwrap().key, '1');

        // Голая клавиша без модификатора — почти наверняка описка, а «нажать
        // просто D» в поле ввода имени вписало бы букву в имя бота.
        assert!(Hotkey::parse("d").is_none());
        // Две клавиши — это не «нажми обе», это опечатка.
        assert!(Hotkey::parse("ctrl+d+e").is_none());
        assert!(Hotkey::parse("ctrl+").is_none());
        assert!(Hotkey::parse("").is_none());
        assert!(Hotkey::parse("ctrl+enter").is_none(), "имена клавиш не поддержаны намеренно");
    }

    /// ГЛАВНЫЙ СТРАЖ ГОРЯЧИХ КЛАВИШ: вслепую переключатель не шлётся.
    ///
    /// `Ctrl+D` — это «поменяй местами», а не «выключи». На неизвестном
    /// состоянии он с равной вероятностью ВКЛЮЧИТ микрофон боту, сидящему в
    /// чужой встрече, — то есть ровно та ошибка, от которой микрофон защищён
    /// двумя барьерами. Разреши эта функция `Unknown` — и второй барьер стал
    /// бы источником поломки вместо защиты.
    #[test]
    fn a_toggle_hotkey_is_never_sent_on_an_unknown_device_state() {
        assert!(!hotkey_allowed(false, DeviceState::Unknown), "«не знаю» — не повод переключать");
        assert!(!hotkey_allowed(true, DeviceState::Unknown));
        // Состояние уже нужное — переключать нечего.
        assert!(!hotkey_allowed(false, DeviceState::Off));
        assert!(!hotkey_allowed(true, DeviceState::On));
        // Известно и противоположно — единственный случай, когда шлём.
        assert!(hotkey_allowed(false, DeviceState::On));
        assert!(hotkey_allowed(true, DeviceState::Off));
    }

    /// Постоянный профиль достаётся ОДНОЙ встрече за раз.
    ///
    /// Chrome не умеет две своих копии на одном `--user-data-dir`, а записей
    /// может идти несколько. Не досталось — одноразовый профиль и гостевой
    /// вход: хуже, но работает, в отличие от двух браузеров на одном каталоге.
    #[test]
    fn the_persistent_profile_is_leased_to_one_meeting_at_a_time() {
        let first = ProfileLease::acquire(true);
        assert!(first.granted());
        let second = ProfileLease::acquire(true);
        assert!(!second.granted(), "две встречи получили один каталог профиля");
        drop(second);
        drop(first);

        // Освободился — снова выдаётся.
        let third = ProfileLease::acquire(true);
        assert!(third.granted());
        drop(third);

        // Не просили — не арендуем и никого не блокируем.
        let idle = ProfileLease::acquire(false);
        assert!(!idle.granted());
        let after = ProfileLease::acquire(true);
        assert!(after.granted(), "невзятая аренда не имеет права держать замок");
    }

    /// Каталог профиля создаётся с правами ТОЛЬКО ДЛЯ ВЛАДЕЛЬЦА.
    ///
    /// Внутри — живая сессия аккаунта. Каталог с `umask` по умолчанию читается
    /// любым процессом пользователя, то есть «войдите один раз руками»
    /// означало бы «положите пароль в общедоступное место».
    #[test]
    fn the_persistent_profile_directory_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = persistent_profile_dir(dir.path());
        ensure_private_dir(&path).unwrap();
        assert!(path.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "профиль виден не только владельцу: {mode:o}");

            // И УЖЕ СУЩЕСТВУЮЩИЙ каталог тоже приводится к 0700: он мог
            // приехать из бэкапа или от прежней версии плагина.
            let mut open = std::fs::metadata(&path).unwrap().permissions();
            open.set_mode(0o755);
            std::fs::set_permissions(&path, open).unwrap();
            ensure_private_dir(&path).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "права существующего каталога не поправлены");
        }
    }

    /// Без `persist_profile` профиль остаётся В КАТАЛОГЕ ВСТРЕЧИ — как до Ф8.
    #[test]
    fn without_the_flag_the_profile_stays_inside_the_meeting_directory() {
        let cfg = ChromeBotConfig {
            url: "https://meet.google.com/abc-defg-hij".into(),
            meeting_dir: PathBuf::from("/tmp/встреча"),
            cache_dir: PathBuf::from("/tmp/кэш"),
            bot_name: "F.O.R.G.E.".into(),
            probes: probes(),
            inject: inject::inject_config(&MeetConfig::default()),
            sample_rate: 16_000,
            window_mode: crate::config::WindowMode::Minimized,
            chrome_path: None,
            ffmpeg_path: None,
            mute_audio: true,
            cdp_command_timeout_ms: 15_000,
            persist_profile: false,
            end: EndConfig::default(),
            timings: BotTimings::default(),
        };
        assert_eq!(
            cfg.launch_config(false).user_data_dir,
            PathBuf::from("/tmp/встреча/chrome-profile"),
            "без флага профиль обязан жить и умирать вместе со встречей"
        );
        assert_eq!(
            cfg.launch_config(true).user_data_dir,
            PathBuf::from("/tmp/кэш/browser-profile"),
            "с арендой профиль обязан пережить встречу — иначе логин теряется"
        );
    }

    /// Скриншот отдаёт картинку даже без браузера: именно в этот момент
    /// человек и смотрит, почему бот не вошёл.
    #[tokio::test]
    async fn a_screenshot_without_a_browser_is_still_a_png() {
        let dir = tempfile::tempdir().unwrap();
        let bot = ChromeBot::new(ChromeBotConfig {
            url: "https://telemost.yandex.ru/j/1".into(),
            meeting_dir: dir.path().join("m"),
            cache_dir: dir.path().join("c"),
            bot_name: "F.O.R.G.E.".into(),
            probes: probes(),
            inject: inject::inject_config(&MeetConfig::default()),
            sample_rate: 16_000,
            window_mode: crate::config::WindowMode::Minimized,
            chrome_path: None,
            ffmpeg_path: None,
            mute_audio: true,
            cdp_command_timeout_ms: 15_000,
            persist_profile: false,
            end: EndConfig::default(),
            timings: BotTimings::default(),
        });
        let png = bot.screenshot().await.unwrap();
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);

        // И наблюдение без страницы отдаёт пустоту, а не ошибку.
        let obs = bot.observe().await.unwrap();
        assert!(obs.participants.is_empty());

        // Повторный стоп — не ошибка.
        bot.stop(StopReason::UserStop).await.unwrap();
        bot.stop(StopReason::Shutdown).await.unwrap();
        assert_eq!(
            bot.actions()
                .iter()
                .filter(|a| matches!(a, BotAction::Stop(_)))
                .count(),
            1
        );
    }
}
