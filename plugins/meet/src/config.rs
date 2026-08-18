//! Конфигурация плагина встреч.
//!
//! # Цепочка источников
//!
//! ```text
//! значения по умолчанию → файл (~/.config/forge/meet.toml) → переменные
//! окружения → validate_and_fix
//! ```
//!
//! [`MeetConfig::load`] **НИКОГДА НЕ ПАНИКУЕТ И НИКОГДА НЕ ВОЗВРАЩАЕТ
//! ОШИБКУ.** Битый TOML не имеет права уронить devforge: он даёт значения по
//! умолчанию плюс `warn` в лог. Причина не в аккуратности, а в том, где живёт
//! этот файл — его правят руками, и опечатка в чужой строке не должна отнимать
//! у пользователя терминал.
//!
//! # Почему таймауты и пороги — поля, а не константы
//!
//! Два независимых довода, и оба не про вкусовщину:
//!
//! 1. **Тесты инжектят миллисекунды.** Автовыход «бот один» в бою ждёт 60
//!    секунд, а тест обязан проверить тот же код за сотню миллисекунд. С
//!    константой это либо `#[ignore]`, либо минута на прогон.
//! 2. **Четыре параметра VAD подбираются только на живых записях.** Порог
//!    включения, порог выключения, подтверждение и hangover — величины,
//!    которые нельзя вывести из первых принципов; их крутят по факту, а правка
//!    константы означает пересборку и релиз.
//!
//! # Герметичность тестов
//!
//! [`config_path`] переопределяется непустым `FORGE_MEET_CONFIG_PATH`,
//! [`cache_dir`] — `FORGE_MEET_CACHE_DIR`. Без обоих `cargo test` читал бы
//! настоящий конфиг разработчика и, что важнее, ходил бы в `~/.forge/meet`,
//! где лежат записи реальных встреч: чужие голоса, расшифровки и саммари.
//! Каждый тест плагина обязан ставить обе переменные (или собирать
//! [`MeetConfig`] структурно, с путями в `TempDir`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// --- Значения по умолчанию ---------------------------------------------------

/// Имя, под которым бот виден участникам встречи.
pub const DEFAULT_BOT_NAME: &str = "F.O.R.G.E.";

/// Автовыход «бот остался один», секунды.
///
/// Минута — компромисс: участник может выйти и вернуться (перезагрузка
/// вкладки, переподключение сети), но час писать пустую комнату незачем.
pub const DEFAULT_ALONE_TIMEOUT_SECS: u64 = 60;

/// Автовыход «тишина при не более чем одном собеседнике», секунды.
pub const DEFAULT_SILENCE_TIMEOUT_SECS: u64 = 900;

/// Потолок длительности одной встречи, секунды (4 часа).
///
/// Последний рубеж против бота, который не заметил конца встречи: без него
/// зависший Chrome писал бы тишину сутками и съел бы диск.
pub const DEFAULT_MAX_MEETING_SECS: u64 = 4 * 60 * 60;

/// Таймаут одной команды CDP, миллисекунды.
pub const DEFAULT_CDP_COMMAND_TIMEOUT_MS: u64 = 15_000;

/// Порог ВКЛЮЧЕНИЯ VAD — децибелы над адаптивным шумовым полом.
pub const DEFAULT_VAD_ON_DB: f32 = 12.0;

/// Порог ВЫКЛЮЧЕНИЯ VAD, дБ над тем же полом. Строго меньше порога включения:
/// гистерезис — единственное, что не даёт границе речи дребезжать.
pub const DEFAULT_VAD_OFF_DB: f32 = 6.0;

/// Сколько миллисекунд звук обязан держаться выше порога, чтобы считаться
/// речью. Момент перехода эмитится СО СДВИГОМ НАЗАД на эту величину — иначе
/// все интервалы систематически съезжали бы вправо.
pub const DEFAULT_VAD_CONFIRM_MS: u64 = 120;

/// Сколько миллисекунд речь считается продолжающейся после падения ниже
/// порога выключения (пауза между словами — не конец реплики).
pub const DEFAULT_VAD_HANGOVER_MS: u64 = 500;

/// Сколько встреч можно писать одновременно.
///
/// Переполнение даёт `Busy` НЕМЕДЛЕННО, без очереди: встреча идёт прямо
/// сейчас, и «подождите освобождения слота» означает, что бот войдёт, когда
/// всё уже кончилось.
pub const DEFAULT_MAX_CONCURRENT_RECORDINGS: usize = 2;

/// Сколько процессов whisper можно держать одновременно. **ЖЁСТКО ОДИН, и
/// [`MeetConfig::validate_and_fix`] возвращает сюда любое другое значение.**
///
/// Metal занимается процессом целиком: два прогона дерутся за GPU и суммарно
/// идут МЕДЛЕННЕЕ одного. Поле существует только затем, чтобы величину было
/// видно в `GET /api/meet/status` рядом с остальными лимитами.
pub const WHISPER_CONCURRENCY: usize = 1;

/// Сколько саммари можно считать одновременно. Переполнение — FIFO-очередь:
/// в реальном времени саммари никто не ждёт.
pub const DEFAULT_MAX_CONCURRENT_SUMMARIES: usize = 2;

/// Частота дискретизации звука по проводу. **Ровно 16 кГц** —
/// [`MeetConfig::validate_and_fix`] чинит любое другое значение.
///
/// Столько хочет whisper. Совпадение частот убирает ffmpeg с критического
/// пути расшифровки: без ffmpeg не будет только архивного файла.
pub const AUDIO_SAMPLE_RATE: u32 = 16_000;

/// Длина одного кадра PCM по проводу, миллисекунды.
pub const DEFAULT_AUDIO_FRAME_MS: u64 = 20;

/// Модель whisper по умолчанию. Ф0 измерила на этой модели 13,3–14,7×
/// реального времени на Apple M1 Max.
pub const DEFAULT_WHISPER_MODEL: &str = "large-v3-turbo-q5_0";

/// Язык распознавания по умолчанию.
pub const DEFAULT_WHISPER_LANGUAGE: &str = "ru";

/// Таймаут одного прогона whisper, секунды.
///
/// Час аудио считается около пяти минут (Ф0), потолок встречи — четыре часа,
/// то есть штатный максимум около двадцати минут. Час даёт четырёхкратный
/// запас и при этом не даёт зависшему процессу занимать GPU до перезапуска.
pub const DEFAULT_WHISPER_TIMEOUT_SECS: u64 = 3600;

/// Таймаут запроса саммари к Claude CLI, секунды.
pub const DEFAULT_SUMMARY_TIMEOUT_SECS: u64 = 600;

/// Потолок длины транскрипта, уезжающего в саммари, символы (≈3 часа речи).
/// Дальше — обрезка с явным маркером.
pub const DEFAULT_SUMMARY_MAX_CHARS: usize = 120_000;

/// Дисковый бюджет каталога встреч, байты (16 ГиБ). `0` — «не подрезать».
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Сколько дней хранить завершённые встречи. `0` — «хранить вечно».
pub const DEFAULT_RETENTION_DAYS: u64 = 90;

/// Сколько ПОСЛЕДНИХ записей ретенция не удаляет никогда.
///
/// Страховка от единственной настройки, способной стереть всё разом:
/// пользователь, поставивший `max_cache_bytes = 100000000` на машине с
/// часовыми встречами, иначе получил бы пустой каталог. Свежие записи — ровно
/// те, ради которых фича и работает, и упёршийся дисковый бюджет не повод
/// стирать вчерашнюю планёрку.
pub const DEFAULT_RETENTION_MIN_KEEP: usize = 5;

/// Сколько секунд записанного звука делают запись достойной спасения после
/// перезапуска devforge (см. [`crate::reconcile`]).
pub const DEFAULT_SALVAGE_MIN_SECS: u64 = 10;

/// Сколько ждать ответа пробы внешнего бинаря, секунд.
///
/// Тридцать, а не пять: ПЕРВЫЙ В ЖИЗНИ запуск `whisper-cli` после установки
/// тратит 12,5 секунды на `ggml_metal_library_init` (измерено в Ф0). Короткий
/// таймаут дал бы на свежей машине ложное «whisper недоступен» — и человек
/// чинил бы то, что не сломано.
pub const DEFAULT_DEPS_PROBE_TIMEOUT_SECS: u64 = 30;

/// Потолок одного запуска ffmpeg, секунд. Часовая встреча сводится единицы
/// секунд; пятнадцать минут — это уже зависший процесс.
pub const DEFAULT_FFMPEG_TIMEOUT_SECS: u64 = 900;

/// Сколько ждать строку `ggml_metal_init:` в stderr прогона whisper, секунд.
///
/// Не «сколько всего работает whisper», а «сколько ждём ДОКАЗАТЕЛЬСТВА, что
/// расчёт пошёл на GPU». Порог щедрый по той же причине, что и у пробы:
/// перед этой строкой идёт `ggml_metal_library_init`, который на холодной
/// машине занимает 12,5 секунды. Не дождались — процесс убивается с
/// `metal_unavailable`, а не досчитывает молча на CPU.
pub const DEFAULT_METAL_GRACE_SECS: u64 = 60;

/// Как часто пайплайн проверяет, не идёт ли запись, прежде чем звать whisper.
///
/// Whisper НЕ ЗАПУСКАЕТСЯ, пока пишется хоть одна встреча: Chrome рендерит её
/// на том же GPU и начнёт получать дропы — то есть испортится звук ровно у
/// той записи, которая идёт прямо сейчас.
pub const DEFAULT_WHISPER_GATE_POLL_MS: u64 = 250;

/// Интервалы речи короче этого выбрасываются, мс.
pub const DEFAULT_SPEAKER_MIN_INTERVAL_MS: u64 = 250;

/// Соседние интервалы ОДНОГО спикера с зазором меньше этого сливаются, мс.
pub const DEFAULT_SPEAKER_MERGE_GAP_MS: u64 = 400;

/// Ниже этой уверенности сегмент считается спорным.
pub const DEFAULT_SPEAKER_MIN_CONFIDENCE: f64 = 0.6;

/// Доля второго претендента, при которой спорный сегмент режется по
/// потокенным меткам и помечается `overlapped`.
pub const DEFAULT_SPEAKER_SECOND_SHARE: f64 = 0.25;

/// Расхождение таймлайна событий с фактическим аудио, начиная с которого
/// применяется линейная коррекция, мс.
pub const DEFAULT_DRIFT_THRESHOLD_MS: u64 = 1_000;

/// Окно корреляции «VAD дорожки ↔ DOM-индикатор говорящего», секунды.
///
/// Третья ступень каскада привязки имён ([`crate::bot::attrib`]) сравнивает
/// таймлайны на первых девяноста секундах встречи: за это время высказываются
/// все, а платить сравнением за час записи незачем.
pub const DEFAULT_SPEAKER_CORRELATION_WINDOW_SECS: u64 = 90;

/// Порог принятия назначения при корреляции.
pub const DEFAULT_SPEAKER_CORRELATION_MIN_SCORE: f64 = 0.7;

/// Обязательный зазор между лучшим и вторым претендентом при корреляции.
///
/// Без него порог пропускал бы НЕОДНОЗНАЧНЫЕ случаи: две дорожки, одинаково
/// хорошо ложащиеся на одного человека, получили бы имя наугад. Неверное имя в
/// транскрипте хуже отсутствующего — его не видно.
pub const DEFAULT_SPEAKER_CORRELATION_MIN_MARGIN: f64 = 0.1;

// --- Перечисления конфига ----------------------------------------------------

/// Где живёт окно бота.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    /// За экраном. Headful, то есть окно МОЖНО показать человеку — это
    /// несущая функция ассистированного входа, а не украшение.
    ///
    /// **НЕ ПРЯЧЕТ ОКНО НА macOS, И ЭТО ИЗМЕРЕНО ДВАЖДЫ.** Запрос
    /// `left: -4000` для окна 1280×800 оконный сервер подрезает: Ф1 получила
    /// фактические `left: -930, top: 30` (полоса около 350 пикселей поверх
    /// работы пользователя), повторное измерение Ф3 на том же Chrome 151 —
    /// `left: 310`, то есть окно целиком на виду. Так же клампится и
    /// `--window-position` при запуске. Режим оставлен опцией для систем, где
    /// координаты не подрезаются.
    Offscreen,
    /// Свёрнутое окно — **ДЕФОЛТ**. Исчезает с экрана ЦЕЛИКОМ и остаётся
    /// headful, то есть его по-прежнему можно показать человеку.
    ///
    /// Риск был в том, что свёрнутое окно occluded, а Chrome троттлит таймеры
    /// рендерера occluded-окна до секунды. Риск СНЯТ ИЗМЕРЕНИЕМ, а не
    /// рассуждением (стенд `plugins/meet/tests/window_mode.rs`, macOS 15,
    /// Chrome 151.0.7922.138, три дорожки по 50 кадров в секунду):
    ///
    /// | величина | свёрнутое, 5 мин | за экраном, 1 мин |
    /// |---|---|---|
    /// | кадров в секунду | 149,9 из 150 теоретических | 148,8 |
    /// | максимальный разрыв между кадрами | 79 мс | 44 мс |
    /// | разрывов сэмпловых смещений (дырок в WAV) | 0 | 0 |
    /// | `Page.captureScreenshot` | работает | работает |
    ///
    /// Троттлинга нет: провал таймеров дал бы разрывы около 1000 мс и
    /// частоту в разы ниже. Причина — VAD и нарезка кадров живут в
    /// `AudioWorkletProcessor`, то есть в аудиопотоке, который не подвержен
    /// троттлингу в принципе; три флага `--disable-*-throttling` остаются
    /// вторым эшелоном.
    #[default]
    Minimized,
    /// Видимое окно — отладка и ассистированный вход.
    Visible,
    /// `--headless=new`. Даёт UA с `HeadlessChrome` (повод для антибот-отказа)
    /// и непроверенную зону с WebRTC-аудио на macOS. Оставлен как запасной
    /// путь для машин без GUI-сессии.
    Headless,
}

impl WindowMode {
    /// Разбор значения из TOML/env. Неизвестное значение — `None`, вызывающий
    /// чинит его на дефолт и пишет в лог.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "offscreen" => Some(Self::Offscreen),
            "minimized" => Some(Self::Minimized),
            "visible" => Some(Self::Visible),
            "headless" => Some(Self::Headless),
            _ => None,
        }
    }

    /// Строковая форма — она же уезжает в `GET /api/meet/status`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offscreen => "offscreen",
            Self::Minimized => "minimized",
            Self::Visible => "visible",
            Self::Headless => "headless",
        }
    }
}

/// Кодек звука по проводу между инжектированным скриптом и плагином.
///
/// Живёт полем конфига, а не константой, потому что прячется за
/// `trait AudioTransport`: если живая проба покажет, что 213 КБ/с base64 по
/// CDP — много, переключение на Opus обязано быть сменой значения, а не
/// переделкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    /// Пер-трековый PCM16 16 кГц моно. Адресуется СЭМПЛОВЫМ СМЕЩЕНИЕМ:
    /// чанк пришёл не по порядку — кладём по смещению, чанк потерян — там
    /// остаётся тишина, таймлайн не сдвигается.
    #[default]
    Pcm16,
    /// Opus в контейнере WebM. Дешевле по проводу, но `MediaRecorder` кладёт
    /// EBML-заголовок только в первый чанк: потеря одного чанка делает
    /// невалидным весь хвост файла.
    OpusWebm,
}

impl AudioCodec {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pcm16" => Some(Self::Pcm16),
            "opus_webm" | "opus" => Some(Self::OpusWebm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pcm16 => "pcm16",
            Self::OpusWebm => "opus_webm",
        }
    }
}

/// Транспорт звука со страницы в плагин.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioTransport {
    /// `Runtime.addBinding` → `window.__forge_<rand32>(frame)` →
    /// `Runtime.bindingCalled`. **Единственный вариант, у которого нет ни
    /// одного из четырёх сетевых барьеров** (csrf_guard, Bearer, CSP
    /// `connect-src`, Private Network Access).
    #[default]
    CdpBinding,
    /// Бинарный loopback-сокет. Запасной путь, если биндинг окажется узким
    /// местом; в нём барьеры возвращаются и решаются отдельно.
    LoopbackSocket,
}

impl AudioTransport {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cdp_binding" | "cdp" => Some(Self::CdpBinding),
            "loopback_socket" | "loopback" => Some(Self::LoopbackSocket),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CdpBinding => "cdp_binding",
            Self::LoopbackSocket => "loopback_socket",
        }
    }
}

// --- Пути --------------------------------------------------------------------

/// Дефолтный путь TOML-конфига (`~/.config/forge/meet.toml`).
pub fn default_config_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config/forge/meet.toml"),
        None => PathBuf::from("./meet.toml"),
    }
}

/// Фактический путь конфига: непустой `FORGE_MEET_CONFIG_PATH` побеждает.
///
/// Без этой переменной любой `cargo test`, дёрнувший [`MeetConfig::load`],
/// читал бы живой конфиг разработчика.
pub fn config_path() -> PathBuf {
    match std::env::var_os("FORGE_MEET_CONFIG_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_config_path(),
    }
}

/// Дефолтный корень данных плагина (`~/.forge/meet`).
///
/// Внутри: `<id>/` на встречу (`manifest.json`, `events.ndjson`,
/// `raw/track-N.wav`, `chrome.pid`, `work/`, `transcript.*`, `summary.md`),
/// `models/` с весами whisper и `meet.db`.
pub fn default_cache_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".forge/meet"),
        None => PathBuf::from("./forge-meet"),
    }
}

/// Фактический корень данных: непустой `FORGE_MEET_CACHE_DIR` побеждает.
///
/// **Самая опасная переменная плагина.** Без неё тест, создавший встречу,
/// писал бы в каталог с реальными записями пользователя — чужими голосами и
/// расшифровками.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("FORGE_MEET_CACHE_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_cache_dir(),
    }
}

/// Дефолтный путь бинаря Claude CLI (`$HOME/.local/bin/claude`).
///
/// Тот же дефолт, что у Echo, VK, Telegram и Bitrix: все они запускают ОДИН
/// И ТОТ ЖЕ CLI, и разные умолчания означали бы, что чат Echo работает, а
/// саммари встречи — нет, без единого сообщения о причине.
pub fn default_claude_cli_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".local/bin/claude"),
        None => PathBuf::from("claude"),
    }
}

// --- Сам конфиг --------------------------------------------------------------

/// Конфигурация плагина встреч.
///
/// Строится только через [`MeetConfig::load`] (боевой путь) либо структурно
/// (тесты — с путями в `TempDir`).
#[derive(Debug, Clone, PartialEq)]
pub struct MeetConfig {
    // --- пути ---
    /// Откуда прочитан TOML. Хранится, чтобы `GET /api/meet/config` мог
    /// честно показать источник, а не гадать заново.
    pub config_path: PathBuf,
    /// Корень данных плагина.
    pub cache_dir: PathBuf,
    /// Файл SQLite. По умолчанию `<cache_dir>/meet.db`.
    pub db_path: PathBuf,
    /// Каталог весов whisper. По умолчанию `<cache_dir>/models`.
    pub models_dir: PathBuf,

    // --- бот ---
    /// Имя, под которым бот виден участникам.
    pub bot_name: String,
    /// Где живёт окно бота.
    pub window_mode: WindowMode,
    /// Явный путь к Chrome; `None` — искать по списку стандартных мест.
    pub chrome_path: Option<PathBuf>,
    /// Глушить ли звук встречи в колонках (`--mute-audio`).
    ///
    /// Поле, а не константа: гейт Ф0 сделал вопрос «не глушит ли этот флаг
    /// данные для WebAudio» приёмочным пунктом Ф3, и измерение не должно
    /// требовать правки кода.
    pub mute_audio: bool,
    /// Постоянный профиль браузера вместо одноразового.
    ///
    /// # Зачем это вообще есть
    ///
    /// Google Meet при ограниченном доступе хоста ОТКЛОНЯЕТ заявку анонимного
    /// участника автоматически. Единственный обход — заходить залогиненным, а
    /// для этого сессия Google обязана пережить встречу: одноразовый профиль
    /// лежит в каталоге встречи и умирает вместе с ней. Пользователь один раз
    /// входит в аккаунт руками (ассистированный вход с видимым окном), дальше
    /// профиль переиспользуется всеми встречами.
    ///
    /// # Почему по умолчанию ВЫКЛЮЧЕНО
    ///
    /// Потому что это меняет свойства хранения: в каталоге профиля лежит
    /// ЖИВАЯ СЕССИЯ аккаунта, то есть по сути пароль. Настройка, сохраняющая
    /// учётные данные на диск, не имеет права включаться сама. Каталог
    /// создаётся с правами `0700` (см. [`crate::bot::ensure_private_dir`]), а
    /// сбросить его можно одним запросом `DELETE /api/meet/profile`.
    pub persist_profile: bool,

    // --- автовыход ---
    pub alone_timeout_secs: u64,
    pub silence_timeout_secs: u64,
    pub max_meeting_secs: u64,
    pub cdp_command_timeout_ms: u64,

    // --- VAD ---
    pub vad_on_db: f32,
    pub vad_off_db: f32,
    pub vad_confirm_ms: u64,
    pub vad_hangover_ms: u64,

    // --- лимиты параллелизма ---
    pub max_concurrent_recordings: usize,
    /// Всегда 1 после [`MeetConfig::validate_and_fix`], см. [`WHISPER_CONCURRENCY`].
    pub max_concurrent_whisper: usize,
    pub max_concurrent_summaries: usize,

    // --- аудио ---
    pub audio_codec: AudioCodec,
    pub audio_transport: AudioTransport,
    /// Всегда [`AUDIO_SAMPLE_RATE`] после `validate_and_fix`.
    pub audio_sample_rate: u32,
    pub audio_frame_ms: u64,

    // --- расшифровка ---
    pub whisper_model: String,
    pub whisper_language: String,
    pub whisper_timeout_secs: u64,
    /// Явный путь к бинарю whisper.cpp; `None` — перебор кандидатов
    /// (`whisper-cli` → `whisper-cpp` → `main` → `whisper`, последний только
    /// с верификацией `--version`: под именем `whisper` в PATH часто лежит
    /// Python openai-whisper, у которого CPU по умолчанию).
    pub whisper_path: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
    pub ffprobe_path: Option<PathBuf>,
    /// Расшифровывать ли дорожки по отдельности, вырезав речь по VAD.
    ///
    /// Действует ТОЛЬКО в режиме
    /// [`crate::bot::mode::TrackMode::PerParticipant`]; в `Slots` (Телемост) и
    /// `Mixed` дорожка не соответствует человеку, и резать по ней нечего — там
    /// расшифровывается микс независимо от этого флага. Выключатель нужен на
    /// случай, когда детектор режима ошибся: без него единственным лечением
    /// была бы пересборка.
    pub pertrack_transcription: bool,

    // --- саммари ---
    pub summary_enabled: bool,
    pub claude_cli_path: PathBuf,
    pub summary_timeout_secs: u64,
    pub summary_max_chars: usize,

    // --- ретенция ---
    pub max_cache_bytes: u64,
    pub retention_days: u64,
    /// Сколько последних записей не удаляются НИКОГДА, см.
    /// [`DEFAULT_RETENTION_MIN_KEEP`].
    pub retention_min_keep: usize,

    // --- пост-обработка ---
    /// Таймаут пробы внешнего бинаря, секунд.
    pub deps_probe_timeout_secs: u64,
    /// Потолок одного запуска ffmpeg, секунд.
    pub ffmpeg_timeout_secs: u64,
    /// Сколько ждать `ggml_metal_init:` в stderr whisper, секунд.
    pub metal_grace_secs: u64,
    /// Период опроса «идёт ли запись» перед стартом whisper, мс.
    pub whisper_gate_poll_ms: u64,
    /// Порог отбрасывания коротких интервалов речи, мс.
    pub speaker_min_interval_ms: u64,
    /// Зазор слияния соседних интервалов одного спикера, мс.
    pub speaker_merge_gap_ms: u64,
    /// Порог уверенности атрибуции.
    pub speaker_min_confidence: f64,
    /// Доля второго претендента, при которой сегмент режется.
    pub speaker_second_share: f64,
    /// Порог применения линейной коррекции дрейфа, мс.
    pub drift_threshold_ms: u64,
    /// Окно корреляции таймлайнов при привязке имён, секунды.
    pub speaker_correlation_window_secs: u64,
    /// Порог принятия назначения при корреляции.
    pub speaker_correlation_min_score: f64,
    /// Зазор до второго претендента при корреляции.
    pub speaker_correlation_min_margin: f64,

    // --- восстановление после перезапуска ---
    /// Сколько секунд звука на диске делают запись спасаемой. См.
    /// [`crate::reconcile::reconcile_on_boot`].
    pub salvage_min_secs: u64,
}

impl Default for MeetConfig {
    fn default() -> Self {
        let cache = cache_dir();
        Self {
            config_path: config_path(),
            db_path: cache.join("meet.db"),
            models_dir: cache.join("models"),
            cache_dir: cache,

            bot_name: DEFAULT_BOT_NAME.to_string(),
            window_mode: WindowMode::default(),
            chrome_path: None,
            mute_audio: true,
            persist_profile: false,

            alone_timeout_secs: DEFAULT_ALONE_TIMEOUT_SECS,
            silence_timeout_secs: DEFAULT_SILENCE_TIMEOUT_SECS,
            max_meeting_secs: DEFAULT_MAX_MEETING_SECS,
            cdp_command_timeout_ms: DEFAULT_CDP_COMMAND_TIMEOUT_MS,

            vad_on_db: DEFAULT_VAD_ON_DB,
            vad_off_db: DEFAULT_VAD_OFF_DB,
            vad_confirm_ms: DEFAULT_VAD_CONFIRM_MS,
            vad_hangover_ms: DEFAULT_VAD_HANGOVER_MS,

            max_concurrent_recordings: DEFAULT_MAX_CONCURRENT_RECORDINGS,
            max_concurrent_whisper: WHISPER_CONCURRENCY,
            max_concurrent_summaries: DEFAULT_MAX_CONCURRENT_SUMMARIES,

            audio_codec: AudioCodec::default(),
            audio_transport: AudioTransport::default(),
            audio_sample_rate: AUDIO_SAMPLE_RATE,
            audio_frame_ms: DEFAULT_AUDIO_FRAME_MS,

            whisper_model: DEFAULT_WHISPER_MODEL.to_string(),
            whisper_language: DEFAULT_WHISPER_LANGUAGE.to_string(),
            whisper_timeout_secs: DEFAULT_WHISPER_TIMEOUT_SECS,
            whisper_path: None,
            ffmpeg_path: None,
            ffprobe_path: None,
            pertrack_transcription: true,

            summary_enabled: true,
            claude_cli_path: default_claude_cli_path(),
            summary_timeout_secs: DEFAULT_SUMMARY_TIMEOUT_SECS,
            summary_max_chars: DEFAULT_SUMMARY_MAX_CHARS,

            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            retention_days: DEFAULT_RETENTION_DAYS,
            retention_min_keep: DEFAULT_RETENTION_MIN_KEEP,

            deps_probe_timeout_secs: DEFAULT_DEPS_PROBE_TIMEOUT_SECS,
            ffmpeg_timeout_secs: DEFAULT_FFMPEG_TIMEOUT_SECS,
            metal_grace_secs: DEFAULT_METAL_GRACE_SECS,
            whisper_gate_poll_ms: DEFAULT_WHISPER_GATE_POLL_MS,
            speaker_min_interval_ms: DEFAULT_SPEAKER_MIN_INTERVAL_MS,
            speaker_merge_gap_ms: DEFAULT_SPEAKER_MERGE_GAP_MS,
            speaker_min_confidence: DEFAULT_SPEAKER_MIN_CONFIDENCE,
            speaker_second_share: DEFAULT_SPEAKER_SECOND_SHARE,
            drift_threshold_ms: DEFAULT_DRIFT_THRESHOLD_MS,
            speaker_correlation_window_secs: DEFAULT_SPEAKER_CORRELATION_WINDOW_SECS,
            speaker_correlation_min_score: DEFAULT_SPEAKER_CORRELATION_MIN_SCORE,
            speaker_correlation_min_margin: DEFAULT_SPEAKER_CORRELATION_MIN_MARGIN,

            salvage_min_secs: DEFAULT_SALVAGE_MIN_SECS,
        }
    }
}

/// Все ключи, которые понимает файл конфига.
///
/// Существует ради ДИАГНОСТИКИ опечаток: `bot_nam = "…"` иначе просто ничего
/// не сделал бы, и пользователь час искал бы, почему бот входит под старым
/// именем. Незнакомый ключ даёт `warn` с его именем — и НЕ отменяет остальной
/// файл (`deny_unknown_fields` был бы хуже: одна опечатка обнуляла бы все
/// настройки разом, а после отката версии devforge это происходило бы на
/// ровном месте — от ключа, который сам же более новый devforge и записал).
pub const KNOWN_KEYS: &[&str] = &[
    "cache_dir",
    "db_path",
    "models_dir",
    "bot_name",
    "window_mode",
    "chrome_path",
    "mute_audio",
    "persist_profile",
    "alone_timeout_secs",
    "silence_timeout_secs",
    "max_meeting_secs",
    "cdp_command_timeout_ms",
    "vad_on_db",
    "vad_off_db",
    "vad_confirm_ms",
    "vad_hangover_ms",
    "max_concurrent_recordings",
    "max_concurrent_whisper",
    "max_concurrent_summaries",
    "audio_codec",
    "audio_transport",
    "audio_sample_rate",
    "audio_frame_ms",
    "whisper_model",
    "whisper_language",
    "whisper_timeout_secs",
    "whisper_path",
    "ffmpeg_path",
    "ffprobe_path",
    "pertrack_transcription",
    "summary_enabled",
    "claude_cli_path",
    "summary_timeout_secs",
    "summary_max_chars",
    "max_cache_bytes",
    "retention_days",
    "retention_min_keep",
    "salvage_min_secs",
    "deps_probe_timeout_secs",
    "ffmpeg_timeout_secs",
    "metal_grace_secs",
    "whisper_gate_poll_ms",
    "speaker_min_interval_ms",
    "speaker_merge_gap_ms",
    "speaker_min_confidence",
    "speaker_second_share",
    "drift_threshold_ms",
    "speaker_correlation_window_secs",
    "speaker_correlation_min_score",
    "speaker_correlation_min_margin",
];

/// Проекция TOML-файла: КАЖДОЕ поле необязательно.
///
/// Отдельный тип, а не `#[serde(default)]` на [`MeetConfig`], ровно затем,
/// чтобы отличать «в файле написано значение по умолчанию» от «в файле этого
/// ключа нет»: без различия env-переменная не смогла бы перебить файл.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    cache_dir: Option<PathBuf>,
    db_path: Option<PathBuf>,
    models_dir: Option<PathBuf>,

    bot_name: Option<String>,
    window_mode: Option<String>,
    chrome_path: Option<PathBuf>,
    mute_audio: Option<bool>,
    persist_profile: Option<bool>,

    alone_timeout_secs: Option<u64>,
    silence_timeout_secs: Option<u64>,
    max_meeting_secs: Option<u64>,
    cdp_command_timeout_ms: Option<u64>,

    vad_on_db: Option<f32>,
    vad_off_db: Option<f32>,
    vad_confirm_ms: Option<u64>,
    vad_hangover_ms: Option<u64>,

    max_concurrent_recordings: Option<usize>,
    max_concurrent_whisper: Option<usize>,
    max_concurrent_summaries: Option<usize>,

    audio_codec: Option<String>,
    audio_transport: Option<String>,
    audio_sample_rate: Option<u32>,
    audio_frame_ms: Option<u64>,

    whisper_model: Option<String>,
    whisper_language: Option<String>,
    whisper_timeout_secs: Option<u64>,
    whisper_path: Option<PathBuf>,
    ffmpeg_path: Option<PathBuf>,
    ffprobe_path: Option<PathBuf>,
    pertrack_transcription: Option<bool>,

    summary_enabled: Option<bool>,
    claude_cli_path: Option<PathBuf>,
    summary_timeout_secs: Option<u64>,
    summary_max_chars: Option<usize>,

    max_cache_bytes: Option<u64>,
    retention_days: Option<u64>,
    retention_min_keep: Option<usize>,

    salvage_min_secs: Option<u64>,

    deps_probe_timeout_secs: Option<u64>,
    ffmpeg_timeout_secs: Option<u64>,
    metal_grace_secs: Option<u64>,
    whisper_gate_poll_ms: Option<u64>,
    speaker_min_interval_ms: Option<u64>,
    speaker_merge_gap_ms: Option<u64>,
    speaker_min_confidence: Option<f64>,
    speaker_second_share: Option<f64>,
    drift_threshold_ms: Option<u64>,
    speaker_correlation_window_secs: Option<u64>,
    speaker_correlation_min_score: Option<f64>,
    speaker_correlation_min_margin: Option<f64>,
}

/// Ключи верхнего уровня, которых нет в [`KNOWN_KEYS`].
///
/// Битый TOML даёт пустой список: про синтаксис ругается разбор, и вторая
/// жалоба про «неизвестные ключи» только запутала бы.
pub fn unknown_keys(raw: &str) -> Vec<String> {
    let Ok(table) = raw.parse::<toml::Table>() else {
        return Vec::new();
    };
    table
        .keys()
        .filter(|k| !KNOWN_KEYS.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// Читает env-переменную, игнорируя пустое значение.
fn env_str(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Читает env-переменную и парсит её. Мусор игнорируется с `warn`: падать
/// из-за опечатки в окружении нельзя, а молча принимать её — тем более.
fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    let raw = env_str(key)?;
    match raw.trim().parse::<T>() {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!(
                target: "forge_meet",
                key,
                value = %raw,
                "forge-meet: значение переменной окружения не разбирается, игнорирую"
            );
            None
        }
    }
}

/// Булева env-переменная: `1/true/yes/on` — включено, `0/false/no/off` —
/// выключено, всё остальное игнорируется.
fn env_bool(key: &str) -> Option<bool> {
    let raw = env_str(key)?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            tracing::warn!(
                target: "forge_meet",
                key,
                value = %raw,
                "forge-meet: булева переменная окружения не разбирается, игнорирую"
            );
            None
        }
    }
}

impl MeetConfig {
    /// Полная цепочка `default → файл → env → validate_and_fix`.
    ///
    /// **Не возвращает `Result` намеренно.** Ошибка чтения или разбора файла
    /// — это `warn` и дефолты, а не отказ старта: `meet.toml` правят руками, и
    /// опечатка в нём не должна отнимать у пользователя devforge целиком.
    pub fn load() -> Self {
        let path = config_path();
        let mut cfg = Self::from_file(&path);
        cfg.apply_env();
        for fix in cfg.validate_and_fix() {
            tracing::warn!(target: "forge_meet", fix = %fix, "forge-meet: конфиг поправлен");
        }
        cfg
    }

    /// Дефолты, перекрытые содержимым TOML-файла. Отсутствующий файл — это
    /// НОРМА (плагин ни разу не настраивали), а не ошибка.
    pub fn from_file(path: &Path) -> Self {
        let mut cfg = Self {
            config_path: path.to_path_buf(),
            ..Self::default()
        };

        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return cfg,
            Err(e) => {
                tracing::warn!(
                    target: "forge_meet",
                    path = %path.display(),
                    error = %e,
                    "forge-meet: конфиг не читается, беру значения по умолчанию"
                );
                return cfg;
            }
        };

        for key in unknown_keys(&raw) {
            tracing::warn!(
                target: "forge_meet",
                path = %path.display(),
                key = %key,
                "forge-meet: неизвестный ключ конфига, игнорирую"
            );
        }

        let file: FileConfig = match toml::from_str(&raw) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    target: "forge_meet",
                    path = %path.display(),
                    error = %e,
                    "forge-meet: конфиг не разбирается, беру значения по умолчанию"
                );
                return cfg;
            }
        };

        cfg.apply_file(file);
        cfg
    }

    fn apply_file(&mut self, f: FileConfig) {
        // Пути производных каталогов пересчитываются от cache_dir, но только
        // если сам файл их не задал явно — иначе перенос кэша на другой том
        // уводил бы с собой и БД, и модели, чего пользователь не просил.
        let explicit_db = f.db_path.is_some();
        let explicit_models = f.models_dir.is_some();
        if let Some(v) = f.cache_dir {
            self.cache_dir = v;
            if !explicit_db {
                self.db_path = self.cache_dir.join("meet.db");
            }
            if !explicit_models {
                self.models_dir = self.cache_dir.join("models");
            }
        }
        if let Some(v) = f.db_path {
            self.db_path = v;
        }
        if let Some(v) = f.models_dir {
            self.models_dir = v;
        }

        if let Some(v) = f.bot_name {
            self.bot_name = v;
        }
        if let Some(v) = f.window_mode {
            match WindowMode::parse(&v) {
                Some(m) => self.window_mode = m,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный window_mode, оставляю текущий"
                ),
            }
        }
        if let Some(v) = f.chrome_path {
            self.chrome_path = Some(v);
        }
        if let Some(v) = f.mute_audio {
            self.mute_audio = v;
        }
        if let Some(v) = f.persist_profile {
            self.persist_profile = v;
        }

        if let Some(v) = f.alone_timeout_secs {
            self.alone_timeout_secs = v;
        }
        if let Some(v) = f.silence_timeout_secs {
            self.silence_timeout_secs = v;
        }
        if let Some(v) = f.max_meeting_secs {
            self.max_meeting_secs = v;
        }
        if let Some(v) = f.cdp_command_timeout_ms {
            self.cdp_command_timeout_ms = v;
        }

        if let Some(v) = f.vad_on_db {
            self.vad_on_db = v;
        }
        if let Some(v) = f.vad_off_db {
            self.vad_off_db = v;
        }
        if let Some(v) = f.vad_confirm_ms {
            self.vad_confirm_ms = v;
        }
        if let Some(v) = f.vad_hangover_ms {
            self.vad_hangover_ms = v;
        }

        if let Some(v) = f.max_concurrent_recordings {
            self.max_concurrent_recordings = v;
        }
        if let Some(v) = f.max_concurrent_whisper {
            self.max_concurrent_whisper = v;
        }
        if let Some(v) = f.max_concurrent_summaries {
            self.max_concurrent_summaries = v;
        }

        if let Some(v) = f.audio_codec {
            match AudioCodec::parse(&v) {
                Some(c) => self.audio_codec = c,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный audio_codec, оставляю pcm16"
                ),
            }
        }
        if let Some(v) = f.audio_transport {
            match AudioTransport::parse(&v) {
                Some(t) => self.audio_transport = t,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный audio_transport, оставляю cdp_binding"
                ),
            }
        }
        if let Some(v) = f.audio_sample_rate {
            self.audio_sample_rate = v;
        }
        if let Some(v) = f.audio_frame_ms {
            self.audio_frame_ms = v;
        }

        if let Some(v) = f.whisper_model {
            self.whisper_model = v;
        }
        if let Some(v) = f.whisper_language {
            self.whisper_language = v;
        }
        if let Some(v) = f.whisper_timeout_secs {
            self.whisper_timeout_secs = v;
        }
        if let Some(v) = f.whisper_path {
            self.whisper_path = Some(v);
        }
        if let Some(v) = f.ffmpeg_path {
            self.ffmpeg_path = Some(v);
        }
        if let Some(v) = f.ffprobe_path {
            self.ffprobe_path = Some(v);
        }
        if let Some(v) = f.pertrack_transcription {
            self.pertrack_transcription = v;
        }

        if let Some(v) = f.summary_enabled {
            self.summary_enabled = v;
        }
        if let Some(v) = f.claude_cli_path {
            self.claude_cli_path = v;
        }
        if let Some(v) = f.summary_timeout_secs {
            self.summary_timeout_secs = v;
        }
        if let Some(v) = f.summary_max_chars {
            self.summary_max_chars = v;
        }

        if let Some(v) = f.max_cache_bytes {
            self.max_cache_bytes = v;
        }
        if let Some(v) = f.retention_days {
            self.retention_days = v;
        }
        if let Some(v) = f.retention_min_keep {
            self.retention_min_keep = v;
        }
        if let Some(v) = f.salvage_min_secs {
            self.salvage_min_secs = v;
        }

        if let Some(v) = f.deps_probe_timeout_secs {
            self.deps_probe_timeout_secs = v;
        }
        if let Some(v) = f.ffmpeg_timeout_secs {
            self.ffmpeg_timeout_secs = v;
        }
        if let Some(v) = f.metal_grace_secs {
            self.metal_grace_secs = v;
        }
        if let Some(v) = f.whisper_gate_poll_ms {
            self.whisper_gate_poll_ms = v;
        }
        if let Some(v) = f.speaker_min_interval_ms {
            self.speaker_min_interval_ms = v;
        }
        if let Some(v) = f.speaker_merge_gap_ms {
            self.speaker_merge_gap_ms = v;
        }
        if let Some(v) = f.speaker_min_confidence {
            self.speaker_min_confidence = v;
        }
        if let Some(v) = f.speaker_second_share {
            self.speaker_second_share = v;
        }
        if let Some(v) = f.drift_threshold_ms {
            self.drift_threshold_ms = v;
        }
        if let Some(v) = f.speaker_correlation_window_secs {
            self.speaker_correlation_window_secs = v;
        }
        if let Some(v) = f.speaker_correlation_min_score {
            self.speaker_correlation_min_score = v;
        }
        if let Some(v) = f.speaker_correlation_min_margin {
            self.speaker_correlation_min_margin = v;
        }
    }

    /// Перекрывает значения переменными окружения (высший приоритет после
    /// [`validate_and_fix`]).
    pub fn apply_env(&mut self) {
        // Корень данных: смена через env тянет за собой БД и модели, если те
        // не заданы своими переменными. Иначе тест с `FORGE_MEET_CACHE_DIR` в
        // `TempDir` всё равно открывал бы живой `~/.forge/meet/meet.db`.
        let explicit_db = env_str("FORGE_MEET_DB_PATH");
        let explicit_models = env_str("FORGE_MEET_MODELS_DIR");
        if let Some(v) = env_str("FORGE_MEET_CACHE_DIR") {
            self.cache_dir = PathBuf::from(v);
            if explicit_db.is_none() {
                self.db_path = self.cache_dir.join("meet.db");
            }
            if explicit_models.is_none() {
                self.models_dir = self.cache_dir.join("models");
            }
        }
        if let Some(v) = explicit_db {
            self.db_path = PathBuf::from(v);
        }
        if let Some(v) = explicit_models {
            self.models_dir = PathBuf::from(v);
        }

        if let Some(v) = env_str("FORGE_MEET_BOT_NAME") {
            self.bot_name = v;
        }
        if let Some(v) = env_str("FORGE_MEET_WINDOW_MODE") {
            match WindowMode::parse(&v) {
                Some(m) => self.window_mode = m,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный FORGE_MEET_WINDOW_MODE, игнорирую"
                ),
            }
        }
        if let Some(v) = env_str("FORGE_MEET_CHROME_PATH") {
            self.chrome_path = Some(PathBuf::from(v));
        }
        if let Some(v) = env_bool("FORGE_MEET_MUTE_AUDIO") {
            self.mute_audio = v;
        }
        if let Some(v) = env_bool("FORGE_MEET_PERSIST_PROFILE") {
            self.persist_profile = v;
        }

        if let Some(v) = env_parse("FORGE_MEET_ALONE_TIMEOUT_SECS") {
            self.alone_timeout_secs = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_SILENCE_TIMEOUT_SECS") {
            self.silence_timeout_secs = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_MAX_MEETING_SECS") {
            self.max_meeting_secs = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_CDP_COMMAND_TIMEOUT_MS") {
            self.cdp_command_timeout_ms = v;
        }

        if let Some(v) = env_parse("FORGE_MEET_VAD_ON_DB") {
            self.vad_on_db = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_VAD_OFF_DB") {
            self.vad_off_db = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_VAD_CONFIRM_MS") {
            self.vad_confirm_ms = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_VAD_HANGOVER_MS") {
            self.vad_hangover_ms = v;
        }

        if let Some(v) = env_parse("FORGE_MEET_MAX_RECORDINGS") {
            self.max_concurrent_recordings = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_MAX_WHISPER") {
            self.max_concurrent_whisper = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_MAX_SUMMARIES") {
            self.max_concurrent_summaries = v;
        }

        if let Some(v) = env_str("FORGE_MEET_AUDIO_CODEC") {
            match AudioCodec::parse(&v) {
                Some(c) => self.audio_codec = c,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный FORGE_MEET_AUDIO_CODEC, игнорирую"
                ),
            }
        }
        if let Some(v) = env_str("FORGE_MEET_AUDIO_TRANSPORT") {
            match AudioTransport::parse(&v) {
                Some(t) => self.audio_transport = t,
                None => tracing::warn!(
                    target: "forge_meet",
                    value = %v,
                    "forge-meet: неизвестный FORGE_MEET_AUDIO_TRANSPORT, игнорирую"
                ),
            }
        }
        if let Some(v) = env_parse("FORGE_MEET_AUDIO_SAMPLE_RATE") {
            self.audio_sample_rate = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_AUDIO_FRAME_MS") {
            self.audio_frame_ms = v;
        }

        if let Some(v) = env_str("FORGE_MEET_WHISPER_MODEL") {
            self.whisper_model = v;
        }
        if let Some(v) = env_str("FORGE_MEET_WHISPER_LANGUAGE") {
            self.whisper_language = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_WHISPER_TIMEOUT_SECS") {
            self.whisper_timeout_secs = v;
        }
        if let Some(v) = env_str("FORGE_MEET_WHISPER_PATH") {
            self.whisper_path = Some(PathBuf::from(v));
        }
        if let Some(v) = env_str("FORGE_MEET_FFMPEG_PATH") {
            self.ffmpeg_path = Some(PathBuf::from(v));
        }
        if let Some(v) = env_str("FORGE_MEET_FFPROBE_PATH") {
            self.ffprobe_path = Some(PathBuf::from(v));
        }
        if let Some(v) = env_bool("FORGE_MEET_PERTRACK") {
            self.pertrack_transcription = v;
        }

        if let Some(v) = env_bool("FORGE_MEET_SUMMARY_ENABLED") {
            self.summary_enabled = v;
        }
        if let Some(v) = env_str("FORGE_MEET_CLAUDE_CLI_PATH") {
            self.claude_cli_path = PathBuf::from(v);
        }
        if let Some(v) = env_parse("FORGE_MEET_SUMMARY_TIMEOUT_SECS") {
            self.summary_timeout_secs = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_SUMMARY_MAX_CHARS") {
            self.summary_max_chars = v;
        }

        if let Some(v) = env_parse("FORGE_MEET_MAX_CACHE_BYTES") {
            self.max_cache_bytes = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_RETENTION_DAYS") {
            self.retention_days = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_RETENTION_MIN_KEEP") {
            self.retention_min_keep = v;
        }
        if let Some(v) = env_parse("FORGE_MEET_SALVAGE_MIN_SECS") {
            self.salvage_min_secs = v;
        }
    }

    /// Чинит значения, выходящие за допустимые границы, на ближайшие
    /// допустимые и возвращает список того, что именно поправлено.
    ///
    /// Возвращаемые строки — ровно то, что [`MeetConfig::load`] пишет в лог
    /// `warn`'ом. Отдельного форматирования для лога нет намеренно: иначе тест
    /// проверял бы одну формулировку, а пользователь читал бы другую.
    ///
    /// **Чинит, а не отвергает.** Плагин обязан стартовать при любом конфиге:
    /// `GET /api/meet/status` — единственный роут, который никогда не падает,
    /// и он же единственный способ узнать, что именно не так.
    pub fn validate_and_fix(&mut self) -> Vec<String> {
        let mut fixes = Vec::new();

        macro_rules! clamp {
            ($field:ident, $lo:expr, $hi:expr) => {
                if self.$field < $lo {
                    fixes.push(format!(
                        "{} = {} меньше минимума {}, поднято до минимума",
                        stringify!($field),
                        self.$field,
                        $lo
                    ));
                    self.$field = $lo;
                } else if self.$field > $hi {
                    fixes.push(format!(
                        "{} = {} больше максимума {}, опущено до максимума",
                        stringify!($field),
                        self.$field,
                        $hi
                    ));
                    self.$field = $hi;
                }
            };
        }

        macro_rules! cap {
            ($field:ident, $hi:expr) => {
                if self.$field > $hi {
                    fixes.push(format!(
                        "{} = {} больше максимума {}, опущено до максимума",
                        stringify!($field),
                        self.$field,
                        $hi
                    ));
                    self.$field = $hi;
                }
            };
        }

        let name = self.bot_name.trim().to_string();
        if name.is_empty() {
            fixes.push(format!(
                "bot_name пуст, взято значение по умолчанию {DEFAULT_BOT_NAME:?}"
            ));
            self.bot_name = DEFAULT_BOT_NAME.to_string();
        } else if name.chars().count() > 64 {
            let cut: String = name.chars().take(64).collect();
            fixes.push(format!("bot_name длиннее 64 символов, обрезано до {cut:?}"));
            self.bot_name = cut;
        } else if name != self.bot_name {
            self.bot_name = name;
        }

        clamp!(alone_timeout_secs, 5, 3600);
        clamp!(silence_timeout_secs, 30, 14_400);
        clamp!(max_meeting_secs, 300, 8 * 60 * 60);
        clamp!(cdp_command_timeout_ms, 500, 120_000);

        clamp!(vad_on_db, 1.0, 60.0);
        clamp!(vad_off_db, 0.5, 60.0);
        // Гистерезис обязан быть гистерезисом: порог выключения строго ниже
        // порога включения, иначе граница речи дребезжит на каждом сэмпле.
        if self.vad_off_db >= self.vad_on_db {
            let fixed = (self.vad_on_db / 2.0).max(0.5);
            fixes.push(format!(
                "vad_off_db = {} не ниже vad_on_db = {}, гистерезиса нет; поставлено {}",
                self.vad_off_db, self.vad_on_db, fixed
            ));
            self.vad_off_db = fixed;
        }
        clamp!(vad_confirm_ms, 10, 2000);
        clamp!(vad_hangover_ms, 50, 5000);

        clamp!(deps_probe_timeout_secs, 1, 300);
        clamp!(ffmpeg_timeout_secs, 5, 24 * 3600);
        clamp!(metal_grace_secs, 1, 600);
        clamp!(whisper_gate_poll_ms, 10, 60_000);
        // Ноль здесь законен («не отбрасывать», «не сливать»), поэтому
        // подрезается только верх: `clamp!` с нулевым минимумом дал бы
        // предупреждение о бессмысленном сравнении беззнакового с нулём.
        cap!(speaker_min_interval_ms, 5_000);
        cap!(speaker_merge_gap_ms, 10_000);
        // Доли — именно доли: единица означала бы «уверен всегда», ноль —
        // «спорным не бывает ничего», и обе крайности выключают разрезание
        // сегмента по границе спикера, ради которого весь `-ojf` и нужен.
        clamp!(speaker_min_confidence, 0.0, 1.0);
        clamp!(speaker_second_share, 0.0, 1.0);
        cap!(drift_threshold_ms, 60_000);

        clamp!(max_concurrent_recordings, 1, 8);
        clamp!(max_concurrent_summaries, 1, 8);
        if self.max_concurrent_whisper != WHISPER_CONCURRENCY {
            fixes.push(format!(
                "max_concurrent_whisper = {} запрещён: Metal занимается процессом целиком, \
                 два прогона суммарно идут медленнее одного; возвращено {}",
                self.max_concurrent_whisper, WHISPER_CONCURRENCY
            ));
            self.max_concurrent_whisper = WHISPER_CONCURRENCY;
        }

        if self.audio_sample_rate != AUDIO_SAMPLE_RATE {
            fixes.push(format!(
                "audio_sample_rate = {} не поддерживается: whisper хочет ровно {} Гц; возвращено",
                self.audio_sample_rate, AUDIO_SAMPLE_RATE
            ));
            self.audio_sample_rate = AUDIO_SAMPLE_RATE;
        }
        clamp!(audio_frame_ms, 10, 100);

        if self.whisper_model.trim().is_empty() {
            fixes.push(format!(
                "whisper_model пуста, взято значение по умолчанию {DEFAULT_WHISPER_MODEL:?}"
            ));
            self.whisper_model = DEFAULT_WHISPER_MODEL.to_string();
        }
        if self.whisper_language.trim().is_empty() {
            fixes.push(format!(
                "whisper_language пуст, взято значение по умолчанию {DEFAULT_WHISPER_LANGUAGE:?}"
            ));
            self.whisper_language = DEFAULT_WHISPER_LANGUAGE.to_string();
        }
        clamp!(whisper_timeout_secs, 60, 24 * 60 * 60);
        clamp!(summary_timeout_secs, 10, 3600);
        clamp!(summary_max_chars, 1000, 500_000);
        // Нижней границы у ретенции нет: `0` — легальное «хранить вечно».
        // Поэтому только потолок, и без `clamp!` — сравнение `u64 < 0` было бы
        // тождественно ложным и заслуженно ловилось бы `unused_comparisons`.
        if self.retention_days > 3650 {
            fixes.push(format!(
                "retention_days = {} больше максимума 3650, опущено до максимума",
                self.retention_days
            ));
            self.retention_days = 3650;
        }
        // У «сколько последних не трогать» нижней границы тоже нет: `0`
        // означает «подчищай всё, что просрочено», и это законный выбор для
        // машины, где записи уезжают в архив другим способом.
        cap!(retention_min_keep, 1000);
        clamp!(salvage_min_secs, 1, 3600);

        fixes
    }

    /// Каталог конкретной встречи: `<cache_dir>/<meeting_id>`.
    pub fn meeting_dir(&self, meeting_id: &str) -> PathBuf {
        self.cache_dir.join(meeting_id)
    }

    /// Создаёт каталоги данных. Ошибка НЕ фатальна: она всплывёт позже, при
    /// реальной записи, с более точным контекстом.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.models_dir)?;
        if let Some(parent) = self.db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Что позволено править из интерфейса
// ---------------------------------------------------------------------------

/// Тип значения поля — им же UI решает, что рисовать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Bool,
    /// Целое неотрицательное.
    Int,
    Float,
    /// Одно из перечисленных в [`EditableField::choices`].
    Choice,
}

/// Поле, которое `PUT /api/meet/config` имеет право записать в файл.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EditableField {
    /// Ключ в `meet.toml` — он же имя поля [`MeetConfig`].
    pub key: &'static str,
    pub kind: FieldKind,
    /// Переменная окружения, перекрывающая ключ. Пустая — такой нет.
    pub env: &'static str,
    /// Раздел формы: `bot` | `transcribe` | `summary` | `timeouts` | `vad` |
    /// `limits` | `storage`.
    pub group: &'static str,
    /// Допустимые значения у [`FieldKind::Choice`].
    pub choices: &'static [&'static str],
    /// Нижняя и верхняя границы у чисел — те же, что чинит
    /// [`MeetConfig::validate_and_fix`]. Уезжают в UI, чтобы человек узнавал о
    /// границе до сохранения, а не из «поправлено» после.
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// **БЕЛЫЙ СПИСОК ПОЛЕЙ, ДОСТУПНЫХ ИЗ ИНТЕРФЕЙСА.**
///
/// # Почему это именно белый список, а не «всё, кроме опасного»
///
/// В [`MeetConfig`] есть пять путей к исполняемым файлам — `chrome_path`,
/// `whisper_path`, `ffmpeg_path`, `ffprobe_path`, `claude_cli_path`. Роут,
/// умеющий записать любой ключ, — это роут, умеющий подменить запускаемый
/// бинарь, то есть выполнение произвольного кода одним PUT'ом. Чёрный список
/// защищал бы от них ровно до появления шестого пути; белый — не пропускает
/// НИКАКОЙ новый ключ, пока автор не впишет его сюда руками.
///
/// Каталоги (`cache_dir`, `db_path`, `models_dir`) не редактируются по другой
/// причине: их смена на живом процессе оставила бы открытую SQLite в старом
/// файле, а записи — в старом каталоге. Это правка файла и перезапуск.
///
/// `max_concurrent_whisper` не редактируется потому, что его единственное
/// законное значение — 1 (Metal занимается процессом целиком).
pub const EDITABLE: &[EditableField] = &[
    EditableField { key: "bot_name", kind: FieldKind::Text, env: "FORGE_MEET_BOT_NAME", group: "bot", choices: &[], min: None, max: None },
    EditableField { key: "window_mode", kind: FieldKind::Choice, env: "FORGE_MEET_WINDOW_MODE", group: "bot", choices: &["offscreen", "minimized", "visible", "headless"], min: None, max: None },
    EditableField { key: "mute_audio", kind: FieldKind::Bool, env: "FORGE_MEET_MUTE_AUDIO", group: "bot", choices: &[], min: None, max: None },
    EditableField { key: "persist_profile", kind: FieldKind::Bool, env: "FORGE_MEET_PERSIST_PROFILE", group: "bot", choices: &[], min: None, max: None },

    EditableField { key: "whisper_model", kind: FieldKind::Text, env: "FORGE_MEET_WHISPER_MODEL", group: "transcribe", choices: &[], min: None, max: None },
    EditableField { key: "whisper_language", kind: FieldKind::Text, env: "FORGE_MEET_WHISPER_LANGUAGE", group: "transcribe", choices: &[], min: None, max: None },
    EditableField { key: "whisper_timeout_secs", kind: FieldKind::Int, env: "FORGE_MEET_WHISPER_TIMEOUT_SECS", group: "transcribe", choices: &[], min: Some(60.0), max: Some(86_400.0) },
    EditableField { key: "pertrack_transcription", kind: FieldKind::Bool, env: "FORGE_MEET_PERTRACK", group: "transcribe", choices: &[], min: None, max: None },

    EditableField { key: "summary_enabled", kind: FieldKind::Bool, env: "FORGE_MEET_SUMMARY_ENABLED", group: "summary", choices: &[], min: None, max: None },
    EditableField { key: "summary_timeout_secs", kind: FieldKind::Int, env: "FORGE_MEET_SUMMARY_TIMEOUT_SECS", group: "summary", choices: &[], min: Some(10.0), max: Some(3600.0) },
    EditableField { key: "summary_max_chars", kind: FieldKind::Int, env: "FORGE_MEET_SUMMARY_MAX_CHARS", group: "summary", choices: &[], min: Some(1000.0), max: Some(500_000.0) },

    EditableField { key: "alone_timeout_secs", kind: FieldKind::Int, env: "FORGE_MEET_ALONE_TIMEOUT_SECS", group: "timeouts", choices: &[], min: Some(5.0), max: Some(3600.0) },
    EditableField { key: "silence_timeout_secs", kind: FieldKind::Int, env: "FORGE_MEET_SILENCE_TIMEOUT_SECS", group: "timeouts", choices: &[], min: Some(30.0), max: Some(14_400.0) },
    EditableField { key: "max_meeting_secs", kind: FieldKind::Int, env: "FORGE_MEET_MAX_MEETING_SECS", group: "timeouts", choices: &[], min: Some(300.0), max: Some(28_800.0) },

    EditableField { key: "vad_on_db", kind: FieldKind::Float, env: "FORGE_MEET_VAD_ON_DB", group: "vad", choices: &[], min: Some(1.0), max: Some(60.0) },
    EditableField { key: "vad_off_db", kind: FieldKind::Float, env: "FORGE_MEET_VAD_OFF_DB", group: "vad", choices: &[], min: Some(0.5), max: Some(60.0) },
    EditableField { key: "vad_confirm_ms", kind: FieldKind::Int, env: "FORGE_MEET_VAD_CONFIRM_MS", group: "vad", choices: &[], min: Some(10.0), max: Some(2000.0) },
    EditableField { key: "vad_hangover_ms", kind: FieldKind::Int, env: "FORGE_MEET_VAD_HANGOVER_MS", group: "vad", choices: &[], min: Some(50.0), max: Some(5000.0) },

    EditableField { key: "max_concurrent_recordings", kind: FieldKind::Int, env: "FORGE_MEET_MAX_RECORDINGS", group: "limits", choices: &[], min: Some(1.0), max: Some(8.0) },
    EditableField { key: "max_concurrent_summaries", kind: FieldKind::Int, env: "FORGE_MEET_MAX_SUMMARIES", group: "limits", choices: &[], min: Some(1.0), max: Some(8.0) },

    EditableField { key: "max_cache_bytes", kind: FieldKind::Int, env: "FORGE_MEET_MAX_CACHE_BYTES", group: "storage", choices: &[], min: Some(0.0), max: None },
    EditableField { key: "retention_days", kind: FieldKind::Int, env: "FORGE_MEET_RETENTION_DAYS", group: "storage", choices: &[], min: Some(0.0), max: Some(3650.0) },
    EditableField { key: "retention_min_keep", kind: FieldKind::Int, env: "FORGE_MEET_RETENTION_MIN_KEEP", group: "storage", choices: &[], min: Some(0.0), max: Some(1000.0) },
];

/// Ключи, которых в [`EDITABLE`] не будет НИКОГДА.
///
/// Список существует ради теста: он ломается, если такой ключ однажды окажется
/// в белом списке «заодно». Пять из них — пути к исполняемым файлам, и цена
/// ошибки здесь не «неудобно», а «PUT запускает произвольный бинарь».
pub const NEVER_EDITABLE: &[&str] = &[
    "chrome_path",
    "whisper_path",
    "ffmpeg_path",
    "ffprobe_path",
    "claude_cli_path",
    "cache_dir",
    "db_path",
    "models_dir",
    "max_concurrent_whisper",
    "audio_sample_rate",
];

/// Описание поля по ключу.
pub fn editable(key: &str) -> Option<&'static EditableField> {
    EDITABLE.iter().find(|f| f.key == key)
}

/// Задан ли ключ переменной окружения ПРЯМО СЕЙЧАС.
///
/// Значения из env файлом не перезаписываются: пользователь, запустивший
/// devforge с `FORGE_MEET_BOT_NAME=…`, ждёт именно это имя, а «сохранил в
/// интерфейсе, а работает старое» — худший вид молчаливого расхождения.
pub fn env_locked(key: &str) -> bool {
    editable(key)
        .filter(|f| !f.env.is_empty())
        .is_some_and(|f| env_str(f.env).is_some())
}

/// Текущее значение редактируемого поля как JSON.
///
/// Пара к [`MeetConfig::apply_editable`]: на согласованность их match'ей есть
/// тест, обходящий [`EDITABLE`] целиком, — иначе ключ, добавленный в список и
/// забытый в одном из двух match'ей, тихо стал бы «только для чтения» либо
/// «только для записи».
pub fn editable_value(cfg: &MeetConfig, key: &str) -> Option<serde_json::Value> {
    use serde_json::json;
    Some(match key {
        "bot_name" => json!(cfg.bot_name),
        "window_mode" => json!(cfg.window_mode.as_str()),
        "mute_audio" => json!(cfg.mute_audio),
        "persist_profile" => json!(cfg.persist_profile),
        "whisper_model" => json!(cfg.whisper_model),
        "whisper_language" => json!(cfg.whisper_language),
        "whisper_timeout_secs" => json!(cfg.whisper_timeout_secs),
        "pertrack_transcription" => json!(cfg.pertrack_transcription),
        "summary_enabled" => json!(cfg.summary_enabled),
        "summary_timeout_secs" => json!(cfg.summary_timeout_secs),
        "summary_max_chars" => json!(cfg.summary_max_chars),
        "alone_timeout_secs" => json!(cfg.alone_timeout_secs),
        "silence_timeout_secs" => json!(cfg.silence_timeout_secs),
        "max_meeting_secs" => json!(cfg.max_meeting_secs),
        "vad_on_db" => json!(cfg.vad_on_db),
        "vad_off_db" => json!(cfg.vad_off_db),
        "vad_confirm_ms" => json!(cfg.vad_confirm_ms),
        "vad_hangover_ms" => json!(cfg.vad_hangover_ms),
        "max_concurrent_recordings" => json!(cfg.max_concurrent_recordings),
        "max_concurrent_summaries" => json!(cfg.max_concurrent_summaries),
        "max_cache_bytes" => json!(cfg.max_cache_bytes),
        "retention_days" => json!(cfg.retention_days),
        "retention_min_keep" => json!(cfg.retention_min_keep),
        _ => return None,
    })
}

impl MeetConfig {
    /// Кладёт значение редактируемого поля. `Err` — тип не тот.
    ///
    /// Границы здесь НЕ проверяются: их чинит [`MeetConfig::validate_and_fix`],
    /// и он же рассказывает, что именно поправил. Вторая проверка границ в
    /// этом месте разошлась бы с первой при первой же правке.
    pub fn apply_editable(&mut self, key: &str, v: &serde_json::Value) -> Result<(), String> {
        let as_u64 = || -> Result<u64, String> {
            v.as_u64()
                .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
                .ok_or_else(|| format!("{key}: ожидалось целое неотрицательное число, пришло {v}"))
        };
        let as_f64 = || -> Result<f64, String> {
            v.as_f64()
                .ok_or_else(|| format!("{key}: ожидалось число, пришло {v}"))
        };
        let as_bool = || -> Result<bool, String> {
            v.as_bool()
                .ok_or_else(|| format!("{key}: ожидалось true или false, пришло {v}"))
        };
        let as_str = || -> Result<String, String> {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{key}: ожидалась строка, пришло {v}"))
        };

        match key {
            "bot_name" => self.bot_name = as_str()?,
            "window_mode" => {
                let raw = as_str()?;
                self.window_mode = WindowMode::parse(&raw)
                    .ok_or_else(|| format!("window_mode: неизвестное значение {raw:?}"))?;
            }
            "mute_audio" => self.mute_audio = as_bool()?,
            "persist_profile" => self.persist_profile = as_bool()?,
            "whisper_model" => self.whisper_model = as_str()?,
            "whisper_language" => self.whisper_language = as_str()?,
            "whisper_timeout_secs" => self.whisper_timeout_secs = as_u64()?,
            "pertrack_transcription" => self.pertrack_transcription = as_bool()?,
            "summary_enabled" => self.summary_enabled = as_bool()?,
            "summary_timeout_secs" => self.summary_timeout_secs = as_u64()?,
            "summary_max_chars" => self.summary_max_chars = as_u64()? as usize,
            "alone_timeout_secs" => self.alone_timeout_secs = as_u64()?,
            "silence_timeout_secs" => self.silence_timeout_secs = as_u64()?,
            "max_meeting_secs" => self.max_meeting_secs = as_u64()?,
            "vad_on_db" => self.vad_on_db = as_f64()? as f32,
            "vad_off_db" => self.vad_off_db = as_f64()? as f32,
            "vad_confirm_ms" => self.vad_confirm_ms = as_u64()?,
            "vad_hangover_ms" => self.vad_hangover_ms = as_u64()?,
            "max_concurrent_recordings" => self.max_concurrent_recordings = as_u64()? as usize,
            "max_concurrent_summaries" => self.max_concurrent_summaries = as_u64()? as usize,
            "max_cache_bytes" => self.max_cache_bytes = as_u64()?,
            "retention_days" => self.retention_days = as_u64()?,
            "retention_min_keep" => self.retention_min_keep = as_u64()? as usize,
            _ => return Err(format!("{key}: поле не редактируется из интерфейса")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Полный файл конфига — все [`KNOWN_KEYS`] со значениями, отличными от
    /// умолчаний. Он же доказывает, что список ключей не разъехался с
    /// [`FileConfig`]: разъедься — и один из `assert_eq!` ниже упадёт.
    const FULL_TOML: &str = r#"
cache_dir = "/tmp/forge-meet-test/cache"
db_path = "/tmp/forge-meet-test/custom.db"
models_dir = "/tmp/forge-meet-test/weights"
bot_name = "Секретарь"
window_mode = "visible"
chrome_path = "/opt/chrome"
mute_audio = false
persist_profile = true
alone_timeout_secs = 90
silence_timeout_secs = 600
max_meeting_secs = 7200
cdp_command_timeout_ms = 20000
vad_on_db = 15.0
vad_off_db = 7.5
vad_confirm_ms = 200
vad_hangover_ms = 700
max_concurrent_recordings = 3
max_concurrent_whisper = 1
max_concurrent_summaries = 4
audio_codec = "opus_webm"
audio_transport = "loopback_socket"
audio_sample_rate = 16000
audio_frame_ms = 40
whisper_model = "medium-q5_0"
whisper_language = "en"
whisper_timeout_secs = 1800
whisper_path = "/opt/whisper-cli"
ffmpeg_path = "/opt/ffmpeg"
ffprobe_path = "/opt/ffprobe"
pertrack_transcription = false
summary_enabled = false
claude_cli_path = "/opt/claude"
summary_timeout_secs = 300
summary_max_chars = 50000
max_cache_bytes = 1073741824
retention_days = 30
retention_min_keep = 3
salvage_min_secs = 20
deps_probe_timeout_secs = 45
ffmpeg_timeout_secs = 600
metal_grace_secs = 20
whisper_gate_poll_ms = 100
speaker_min_interval_ms = 300
speaker_merge_gap_ms = 500
speaker_min_confidence = 0.7
speaker_second_share = 0.3
drift_threshold_ms = 2000
speaker_correlation_window_secs = 120
speaker_correlation_min_score = 0.8
speaker_correlation_min_margin = 0.2
"#;

    /// Каждый тест конфига пишет свои файлы во ВРЕМЕННЫЙ каталог.
    ///
    /// Прочитать чужой `~/.config/forge/meet.toml` было бы полбеды; настоящая
    /// цена — `cache_dir`, где лежат записи реальных встреч пользователя:
    /// чужие голоса, расшифровки и саммари.
    fn hermetic_paths(dir: &std::path::Path) -> MeetConfig {
        MeetConfig {
            config_path: dir.join("meet.toml"),
            cache_dir: dir.join("cache"),
            db_path: dir.join("cache/meet.db"),
            models_dir: dir.join("cache/models"),
            ..MeetConfig::default()
        }
    }

    // --- битый файл --------------------------------------------------------

    /// `load` не имеет права ни паниковать, ни возвращать ошибку: `meet.toml`
    /// правят руками, и опечатка в нём не должна отнимать у пользователя
    /// devforge целиком.
    #[test]
    fn a_broken_toml_yields_defaults_instead_of_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "это [[[ вообще не TOML = = =").unwrap();

        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.bot_name, DEFAULT_BOT_NAME);
        assert_eq!(cfg.window_mode, WindowMode::Minimized, "битый файл обязан дать ДЕФОЛТ");
        assert_eq!(cfg.alone_timeout_secs, DEFAULT_ALONE_TIMEOUT_SECS);
        assert_eq!(cfg.config_path, path, "путь конфига обязан сохраниться");
    }

    #[test]
    fn a_missing_file_is_normal_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("нет-такого.toml");
        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.bot_name, DEFAULT_BOT_NAME);
    }

    /// Файл с ПРАВИЛЬНЫМ синтаксисом, но неверным типом значения — тоже не
    /// повод падать.
    #[test]
    fn a_wrong_value_type_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "alone_timeout_secs = \"минута\"\n").unwrap();
        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.alone_timeout_secs, DEFAULT_ALONE_TIMEOUT_SECS);
    }

    /// Опечатка в имени ключа НЕ обнуляет остальной файл — она лишь называет
    /// себя в логе. Обратное поведение (`deny_unknown_fields`) означало бы,
    /// что откат версии devforge стирает все настройки пользователя из-за
    /// ключа, который сам же более новый devforge и записал.
    #[test]
    fn a_typo_in_a_key_does_not_wipe_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "bot_nam = \"Опечатка\"\nbot_name = \"Верно\"\n").unwrap();

        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.bot_name, "Верно", "правильный ключ обязан примениться");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(unknown_keys(&raw), vec!["bot_nam".to_string()]);
    }

    /// Про битый синтаксис ругается разбор; вторая жалоба про «неизвестные
    /// ключи» только запутала бы.
    #[test]
    fn broken_syntax_produces_no_unknown_key_noise() {
        assert!(unknown_keys("= = =").is_empty());
    }

    /// Список ключей не разъехался с полями: полный файл применяется целиком.
    #[test]
    fn every_known_key_is_actually_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, FULL_TOML).unwrap();

        assert!(
            unknown_keys(FULL_TOML).is_empty(),
            "в полном файле не должно быть незнакомых ключей: {:?}",
            unknown_keys(FULL_TOML)
        );

        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.cache_dir, PathBuf::from("/tmp/forge-meet-test/cache"));
        assert_eq!(cfg.db_path, PathBuf::from("/tmp/forge-meet-test/custom.db"));
        assert_eq!(cfg.models_dir, PathBuf::from("/tmp/forge-meet-test/weights"));
        assert_eq!(cfg.bot_name, "Секретарь");
        assert_eq!(cfg.window_mode, WindowMode::Visible);
        assert_eq!(cfg.chrome_path, Some(PathBuf::from("/opt/chrome")));
        assert!(!cfg.mute_audio);
        assert!(cfg.persist_profile, "постоянный профиль из файла не применился");
        assert_eq!(cfg.alone_timeout_secs, 90);
        assert_eq!(cfg.silence_timeout_secs, 600);
        assert_eq!(cfg.max_meeting_secs, 7200);
        assert_eq!(cfg.cdp_command_timeout_ms, 20_000);
        assert_eq!(cfg.vad_on_db, 15.0);
        assert_eq!(cfg.vad_off_db, 7.5);
        assert_eq!(cfg.vad_confirm_ms, 200);
        assert_eq!(cfg.vad_hangover_ms, 700);
        assert_eq!(cfg.max_concurrent_recordings, 3);
        assert_eq!(cfg.max_concurrent_summaries, 4);
        assert_eq!(cfg.audio_codec, AudioCodec::OpusWebm);
        assert_eq!(cfg.audio_transport, AudioTransport::LoopbackSocket);
        assert_eq!(cfg.audio_frame_ms, 40);
        assert_eq!(cfg.whisper_model, "medium-q5_0");
        assert_eq!(cfg.whisper_language, "en");
        assert_eq!(cfg.whisper_timeout_secs, 1800);
        assert_eq!(cfg.whisper_path, Some(PathBuf::from("/opt/whisper-cli")));
        assert_eq!(cfg.ffmpeg_path, Some(PathBuf::from("/opt/ffmpeg")));
        assert_eq!(cfg.ffprobe_path, Some(PathBuf::from("/opt/ffprobe")));
        assert!(
            !cfg.pertrack_transcription,
            "выключатель пер-трековой расшифровки из файла не применился"
        );
        assert!(!cfg.summary_enabled);
        assert_eq!(cfg.claude_cli_path, PathBuf::from("/opt/claude"));
        assert_eq!(cfg.summary_timeout_secs, 300);
        assert_eq!(cfg.summary_max_chars, 50_000);
        assert_eq!(cfg.max_cache_bytes, 1_073_741_824);
        assert_eq!(cfg.retention_days, 30);
        assert_eq!(cfg.retention_min_keep, 3);
        assert_eq!(cfg.salvage_min_secs, 20);
        // Пороги каскада привязки имён — тоже поля файла, а не константы: их
        // подбирают на живых записях, и правка не должна требовать пересборки.
        assert_eq!(cfg.speaker_correlation_window_secs, 120);
        assert_eq!(cfg.speaker_correlation_min_score, 0.8);
        assert_eq!(cfg.speaker_correlation_min_margin, 0.2);
    }

    /// Умолчания каскада — ровно те, что заложены планом: окно 90 секунд и
    /// порог 0,7.
    #[test]
    fn the_attribution_defaults_match_the_plan() {
        let cfg = MeetConfig::default();
        assert_eq!(cfg.speaker_correlation_window_secs, 90);
        assert_eq!(cfg.speaker_correlation_min_score, 0.7);
        assert!(cfg.speaker_correlation_min_margin > 0.0);
    }

    /// `cache_dir` в файле тянет за собой базу и модели — но только если те
    /// не заданы явно. Иначе перенос кэша на другой том уводил бы с собой и
    /// базу, чего пользователь не просил.
    #[test]
    fn cache_dir_moves_the_derived_paths_unless_they_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");

        std::fs::write(&path, "cache_dir = \"/vol/meet\"\n").unwrap();
        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.db_path, PathBuf::from("/vol/meet/meet.db"));
        assert_eq!(cfg.models_dir, PathBuf::from("/vol/meet/models"));

        std::fs::write(
            &path,
            "cache_dir = \"/vol/meet\"\ndb_path = \"/ssd/meet.db\"\n",
        )
        .unwrap();
        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.db_path, PathBuf::from("/ssd/meet.db"), "явный путь сильнее");
        assert_eq!(cfg.models_dir, PathBuf::from("/vol/meet/models"));
    }

    // --- validate_and_fix --------------------------------------------------

    /// Значения вне диапазона ЧИНЯТСЯ, а не отвергаются: плагин обязан
    /// стартовать при любом конфиге, потому что `GET /api/meet/status` —
    /// единственный способ узнать, что именно не так.
    #[test]
    fn out_of_range_values_are_repaired_and_the_repair_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = MeetConfig {
            alone_timeout_secs: 0,
            silence_timeout_secs: 999_999,
            max_meeting_secs: 1,
            cdp_command_timeout_ms: 1,
            vad_confirm_ms: 0,
            vad_hangover_ms: 0,
            max_concurrent_recordings: 0,
            max_concurrent_summaries: 100,
            audio_frame_ms: 5000,
            summary_max_chars: 10,
            retention_days: 100_000,
            salvage_min_secs: 0,
            ..hermetic_paths(dir.path())
        };
        let fixes = cfg.validate_and_fix();

        assert_eq!(cfg.alone_timeout_secs, 5);
        assert_eq!(cfg.silence_timeout_secs, 14_400);
        assert_eq!(cfg.max_meeting_secs, 300);
        assert_eq!(cfg.cdp_command_timeout_ms, 500);
        assert_eq!(cfg.vad_confirm_ms, 10);
        assert_eq!(cfg.vad_hangover_ms, 50);
        assert_eq!(cfg.max_concurrent_recordings, 1);
        assert_eq!(cfg.max_concurrent_summaries, 8);
        assert_eq!(cfg.audio_frame_ms, 100);
        assert_eq!(cfg.summary_max_chars, 1000);
        assert_eq!(cfg.retention_days, 3650);
        assert_eq!(cfg.salvage_min_secs, 1);

        // Факт починки виден: возвращаемые строки — ровно то, что `load`
        // пишет в лог warn'ом, отдельного форматирования нет намеренно.
        for field in [
            "alone_timeout_secs",
            "silence_timeout_secs",
            "max_meeting_secs",
            "cdp_command_timeout_ms",
            "vad_confirm_ms",
            "vad_hangover_ms",
            "max_concurrent_recordings",
            "max_concurrent_summaries",
            "audio_frame_ms",
            "summary_max_chars",
            "retention_days",
            "salvage_min_secs",
        ] {
            assert!(
                fixes.iter().any(|f| f.contains(field)),
                "починка {field} не попала в отчёт: {fixes:?}"
            );
        }

        // Повторный вызов на уже починенном конфиге не находит ничего.
        assert!(cfg.validate_and_fix().is_empty());
    }

    /// Лимит whisper поднять НЕЛЬЗЯ: Metal занимается процессом целиком, и
    /// два прогона суммарно идут медленнее одного.
    #[test]
    fn the_whisper_limit_is_always_forced_back_to_one() {
        let dir = tempfile::tempdir().unwrap();
        for attempt in [0usize, 2, 4, 64] {
            let mut cfg = MeetConfig {
                max_concurrent_whisper: attempt,
                ..hermetic_paths(dir.path())
            };
            let fixes = cfg.validate_and_fix();
            assert_eq!(cfg.max_concurrent_whisper, WHISPER_CONCURRENCY);
            if attempt != WHISPER_CONCURRENCY {
                assert!(
                    fixes.iter().any(|f| f.contains("max_concurrent_whisper")),
                    "{fixes:?}"
                );
            }
        }
    }

    /// whisper хочет ровно 16 кГц, и совпадение частот убирает ffmpeg с
    /// критического пути расшифровки. Любая другая частота чинится.
    #[test]
    fn the_sample_rate_is_always_forced_back_to_16k() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = MeetConfig {
            audio_sample_rate: 48_000,
            ..hermetic_paths(dir.path())
        };
        let fixes = cfg.validate_and_fix();
        assert_eq!(cfg.audio_sample_rate, AUDIO_SAMPLE_RATE);
        assert!(fixes.iter().any(|f| f.contains("audio_sample_rate")), "{fixes:?}");
    }

    /// Гистерезис обязан быть гистерезисом: без него граница речи дребезжит
    /// на каждом сэмпле.
    #[test]
    fn the_vad_thresholds_keep_their_hysteresis() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = MeetConfig {
            vad_on_db: 10.0,
            vad_off_db: 12.0,
            ..hermetic_paths(dir.path())
        };
        let fixes = cfg.validate_and_fix();
        assert!(cfg.vad_off_db < cfg.vad_on_db, "{cfg:?}");
        assert!(fixes.iter().any(|f| f.contains("vad_off_db")), "{fixes:?}");

        // И равенство порогов — тоже отсутствие гистерезиса.
        let mut cfg = MeetConfig {
            vad_on_db: 10.0,
            vad_off_db: 10.0,
            ..hermetic_paths(dir.path())
        };
        cfg.validate_and_fix();
        assert!(cfg.vad_off_db < cfg.vad_on_db);
    }

    #[test]
    fn an_empty_bot_name_falls_back_and_a_long_one_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = MeetConfig {
            bot_name: "   ".into(),
            ..hermetic_paths(dir.path())
        };
        let fixes = cfg.validate_and_fix();
        assert_eq!(cfg.bot_name, DEFAULT_BOT_NAME);
        assert!(fixes.iter().any(|f| f.contains("bot_name")), "{fixes:?}");

        let mut cfg = MeetConfig {
            // Кириллица: обрезка обязана считать СИМВОЛЫ, а не байты, иначе
            // имя разъехалось бы посреди кодовой точки.
            bot_name: "я".repeat(100),
            ..hermetic_paths(dir.path())
        };
        cfg.validate_and_fix();
        assert_eq!(cfg.bot_name.chars().count(), 64);
    }

    /// Значения по умолчанию сами обязаны быть валидными: иначе каждый старт
    /// без конфига писал бы в лог «поправил».
    #[test]
    fn the_defaults_need_no_repair() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = hermetic_paths(dir.path());
        assert!(cfg.validate_and_fix().is_empty(), "{:?}", cfg.validate_and_fix());
    }

    // --- приоритет источников ----------------------------------------------

    /// default < файл < env. Проверяется на ОДНОМ поле по всей цепочке, чтобы
    /// падение сразу называло сломанную ступень.
    #[test]
    fn the_source_priority_is_default_then_file_then_env() {
        let _guard = crate::testing::env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");

        // 1. Только умолчания.
        std::fs::write(&path, "").unwrap();
        let cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.bot_name, DEFAULT_BOT_NAME);
        assert_eq!(cfg.alone_timeout_secs, DEFAULT_ALONE_TIMEOUT_SECS);

        // 2. Файл перебивает умолчание.
        std::fs::write(&path, "bot_name = \"Из файла\"\nalone_timeout_secs = 120\n").unwrap();
        let mut cfg = MeetConfig::from_file(&path);
        assert_eq!(cfg.bot_name, "Из файла");
        assert_eq!(cfg.alone_timeout_secs, 120);

        // 3. Окружение перебивает файл.
        std::env::set_var("FORGE_MEET_BOT_NAME", "Из окружения");
        std::env::set_var("FORGE_MEET_ALONE_TIMEOUT_SECS", "300");
        cfg.apply_env();
        assert_eq!(cfg.bot_name, "Из окружения");
        assert_eq!(cfg.alone_timeout_secs, 300);
        std::env::remove_var("FORGE_MEET_BOT_NAME");
        std::env::remove_var("FORGE_MEET_ALONE_TIMEOUT_SECS");
    }

    /// ГЕРМЕТИЧНОСТЬ: `FORGE_MEET_CACHE_DIR` уводит за собой базу и модели.
    ///
    /// Без этого свойства тест, поставивший только `FORGE_MEET_CACHE_DIR`,
    /// всё равно открывал бы живой `~/.forge/meet/meet.db` — то есть настоящую
    /// базу встреч пользователя.
    #[test]
    fn the_cache_dir_variable_drags_the_database_and_the_models_with_it() {
        let _guard = crate::testing::env_guard();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("FORGE_MEET_CACHE_DIR", dir.path());

        let mut cfg = MeetConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.cache_dir, dir.path());
        assert_eq!(cfg.db_path, dir.path().join("meet.db"));
        assert_eq!(cfg.models_dir, dir.path().join("models"));

        // Явная переменная базы сильнее.
        let explicit = dir.path().join("elsewhere.db");
        std::env::set_var("FORGE_MEET_DB_PATH", &explicit);
        let mut cfg = MeetConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.db_path, explicit);
        assert_eq!(cfg.models_dir, dir.path().join("models"));

        std::env::remove_var("FORGE_MEET_DB_PATH");
        std::env::remove_var("FORGE_MEET_CACHE_DIR");
    }

    /// `FORGE_MEET_CONFIG_PATH` уводит `load` с боевого пути. Без этого
    /// прогон тестов читал бы живой конфиг разработчика.
    #[test]
    fn the_config_path_variable_redirects_load() {
        let _guard = crate::testing::env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "bot_name = \"Из подменённого пути\"\n").unwrap();

        std::env::set_var("FORGE_MEET_CONFIG_PATH", &path);
        std::env::set_var("FORGE_MEET_CACHE_DIR", dir.path().join("cache"));
        assert_eq!(config_path(), path);

        let cfg = MeetConfig::load();
        assert_eq!(cfg.bot_name, "Из подменённого пути");
        assert_eq!(cfg.cache_dir, dir.path().join("cache"));
        assert!(
            cfg.db_path.starts_with(dir.path()),
            "база обязана остаться во временном каталоге: {}",
            cfg.db_path.display()
        );

        std::env::remove_var("FORGE_MEET_CONFIG_PATH");
        std::env::remove_var("FORGE_MEET_CACHE_DIR");
    }

    /// Мусор в переменной окружения игнорируется, а не роняет старт.
    #[test]
    fn junk_in_the_environment_is_ignored() {
        let _guard = crate::testing::env_guard();
        std::env::set_var("FORGE_MEET_ALONE_TIMEOUT_SECS", "полторы минуты");
        std::env::set_var("FORGE_MEET_MUTE_AUDIO", "возможно");
        std::env::set_var("FORGE_MEET_WINDOW_MODE", "телепорт");

        let mut cfg = MeetConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.alone_timeout_secs, DEFAULT_ALONE_TIMEOUT_SECS);
        assert!(cfg.mute_audio);
        assert_eq!(cfg.window_mode, WindowMode::Minimized, "мусор в переменной оставляет ДЕФОЛТ");

        std::env::remove_var("FORGE_MEET_ALONE_TIMEOUT_SECS");
        std::env::remove_var("FORGE_MEET_MUTE_AUDIO");
        std::env::remove_var("FORGE_MEET_WINDOW_MODE");
    }

    /// Пустое значение переменной трактуется как «не задано»: `VAR=` в
    /// systemd-юните или в `.env` встречается сплошь и рядом и не должно
    /// означать «имя бота — пустая строка».
    #[test]
    fn an_empty_variable_means_unset() {
        let _guard = crate::testing::env_guard();
        std::env::set_var("FORGE_MEET_BOT_NAME", "");
        let mut cfg = MeetConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.bot_name, DEFAULT_BOT_NAME);
        std::env::remove_var("FORGE_MEET_BOT_NAME");
    }

    // --- перечисления ------------------------------------------------------

    /// ДЕФОЛТ ОКНА — СВЁРНУТОЕ, И ЭТО РЕШЕНИЕ ПО ИЗМЕРЕНИЮ.
    ///
    /// `offscreen` не прячет окно на macOS: оконный сервер подрезает
    /// координаты (Ф1 — `left: -930`, повторное измерение Ф3 — `left: 310`,
    /// то есть окно целиком на виду), а требование пользователя — браузер не
    /// мешает работать. Свёрнутое окно исчезает целиком и, по измерению
    /// `tests/window_mode.rs`, не теряет ни кадра: 149,9 к/с из 150 за пять
    /// минут, максимальный разрыв 79 мс, дырок в PCM ноль, скриншот работает.
    ///
    /// Тест держит именно ВЫБОР: смена дефолта обязана быть осознанной и
    /// подкреплённой новым прогоном стенда.
    #[test]
    fn the_default_window_mode_is_minimized_by_measurement() {
        assert_eq!(MeetConfig::default().window_mode, WindowMode::Minimized);
        assert_eq!(WindowMode::default(), WindowMode::Minimized);
    }

    #[test]
    fn the_enum_values_round_trip() {
        for m in [
            WindowMode::Offscreen,
            WindowMode::Minimized,
            WindowMode::Visible,
            WindowMode::Headless,
        ] {
            assert_eq!(WindowMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(WindowMode::parse("OFFSCREEN"), Some(WindowMode::Offscreen));
        assert_eq!(WindowMode::parse("нет такого"), None);

        for c in [AudioCodec::Pcm16, AudioCodec::OpusWebm] {
            assert_eq!(AudioCodec::parse(c.as_str()), Some(c));
        }
        for t in [AudioTransport::CdpBinding, AudioTransport::LoopbackSocket] {
            assert_eq!(AudioTransport::parse(t.as_str()), Some(t));
        }
        // Короткие формы — для человека, который правит файл руками.
        assert_eq!(AudioCodec::parse("opus"), Some(AudioCodec::OpusWebm));
        assert_eq!(AudioTransport::parse("cdp"), Some(AudioTransport::CdpBinding));
    }

    #[test]
    fn ensure_dirs_creates_everything_the_plugin_writes_to() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeetConfig {
            cache_dir: dir.path().join("a/b/cache"),
            models_dir: dir.path().join("a/b/cache/models"),
            db_path: dir.path().join("a/b/db/meet.db"),
            ..MeetConfig::default()
        };
        cfg.ensure_dirs().unwrap();
        assert!(cfg.cache_dir.is_dir());
        assert!(cfg.models_dir.is_dir());
        assert!(cfg.db_path.parent().unwrap().is_dir());
    }

    #[test]
    fn meeting_dir_lives_under_the_cache_root() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = hermetic_paths(dir.path());
        let m = cfg.meeting_dir("abc-123");
        assert_eq!(m, cfg.cache_dir.join("abc-123"));
        assert!(m.starts_with(&cfg.cache_dir));
    }

    // --- белый список редактируемых полей ----------------------------------

    /// **ПУТИ К ИСПОЛНЯЕМЫМ ФАЙЛАМ НЕ РЕДАКТИРУЮТСЯ ЧЕРЕЗ HTTP.**
    ///
    /// Тест не про аккуратность. `PUT`, умеющий записать `claude_cli_path`
    /// или `chrome_path`, — это выполнение произвольного бинаря одним
    /// запросом. Он же ловит и попытку «добавить заодно» новый путь: список
    /// [`NEVER_EDITABLE`] придётся править руками, а это видно в диффе.
    #[test]
    fn no_executable_path_is_editable_from_the_interface() {
        for key in NEVER_EDITABLE {
            assert!(
                editable(key).is_none(),
                "{key} попал в белый список правки из интерфейса"
            );
        }
        for f in EDITABLE {
            assert!(
                !f.key.ends_with("_path") && !f.key.ends_with("_dir"),
                "{}: путь не имеет права быть редактируемым",
                f.key
            );
        }
    }

    /// Каждое редактируемое поле обязано быть известно файлу конфига — иначе
    /// сохранённое значение прочиталось бы как «неизвестный ключ» и молча
    /// пропало при следующем старте.
    #[test]
    fn every_editable_key_is_a_known_config_key() {
        for f in EDITABLE {
            assert!(
                KNOWN_KEYS.contains(&f.key),
                "{} редактируется, но файл конфига про него не знает",
                f.key
            );
        }
        let mut seen = std::collections::HashSet::new();
        for f in EDITABLE {
            assert!(seen.insert(f.key), "дубль ключа {}", f.key);
        }
    }

    /// **ЧТЕНИЕ И ЗАПИСЬ ХОДЯТ ПАРОЙ.**
    ///
    /// Ключ, добавленный в [`EDITABLE`] и забытый в одном из двух match'ей,
    /// стал бы «только для чтения» либо «только для записи» — и то, и другое
    /// выглядит как «настройка не сохраняется», причём без единой ошибки.
    #[test]
    fn every_editable_key_round_trips_through_read_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let base = hermetic_paths(dir.path());

        for f in EDITABLE {
            let current = editable_value(&base, f.key)
                .unwrap_or_else(|| panic!("{}: нет чтения значения", f.key));

            // Значение, ОТЛИЧНОЕ от текущего, и заведомо внутри границ.
            let probe = match f.kind {
                FieldKind::Text => serde_json::json!("проба"),
                FieldKind::Bool => serde_json::json!(!current.as_bool().unwrap()),
                // Первый вариант, ОТЛИЧНЫЙ от текущего: умолчание перечисления
                // меняется (window_mode уже переехал с offscreen на minimized),
                // и жёсткий индекс делал бы тест хрупким на ровном месте.
                FieldKind::Choice => serde_json::json!(f
                    .choices
                    .iter()
                    .find(|c| serde_json::json!(*c) != current)
                    .unwrap_or_else(|| panic!("{}: у перечисления один вариант", f.key))),
                // Числа: первое значение над минимумом, ОТЛИЧНОЕ от текущего.
                // Слепое `min + 1` совпало бы с умолчанием там, где оно и есть
                // «минимум плюс один» (max_concurrent_recordings = 2).
                FieldKind::Int => {
                    let lo = f.min.unwrap_or(0.0).max(0.0) as u64;
                    let v = if serde_json::json!(lo + 1) == current { lo + 2 } else { lo + 1 };
                    assert!(
                        f.max.is_none_or(|hi| (v as f64) <= hi),
                        "{}: проба {v} вылезла за максимум",
                        f.key
                    );
                    serde_json::json!(v)
                }
                FieldKind::Float => {
                    let lo = f.min.unwrap_or(0.0);
                    let v = if serde_json::json!(lo + 1.0) == current { lo + 2.0 } else { lo + 1.0 };
                    serde_json::json!(v)
                }
            };

            let mut cfg = base.clone();
            cfg.apply_editable(f.key, &probe)
                .unwrap_or_else(|e| panic!("{}: запись не удалась: {e}", f.key));
            let back = editable_value(&cfg, f.key).unwrap();
            assert_eq!(
                back, probe,
                "{}: записали {probe}, прочитали {back}",
                f.key
            );
            assert_ne!(back, current, "{}: проба совпала с текущим значением", f.key);
        }
    }

    #[test]
    fn a_value_of_the_wrong_type_is_named_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = hermetic_paths(dir.path());
        for (key, bad) in [
            ("bot_name", serde_json::json!(7)),
            ("summary_enabled", serde_json::json!("да")),
            ("alone_timeout_secs", serde_json::json!("минута")),
            ("alone_timeout_secs", serde_json::json!(-5)),
            ("window_mode", serde_json::json!("невидимое")),
            ("vad_on_db", serde_json::json!("громко")),
        ] {
            let err = cfg.apply_editable(key, &bad).unwrap_err();
            assert!(err.contains(key), "{key}: ошибка без имени поля: {err}");
        }
        // И ключ вне белого списка — тоже ошибка, а не молчаливый пропуск.
        assert!(cfg
            .apply_editable("claude_cli_path", &serde_json::json!("/bin/sh"))
            .is_err());
    }

    /// Переменная окружения ПОБЕЖДАЕТ файл, и роут настроек обязан уметь это
    /// показать. Проверяется на настоящем окружении под общим замком: env —
    /// состояние всего процесса.
    #[test]
    fn env_locked_reports_what_the_environment_holds() {
        let _guard = crate::testing::env_guard();
        let key = "FORGE_MEET_BOT_NAME";
        let saved = std::env::var_os(key);
        std::env::remove_var(key);
        assert!(!env_locked("bot_name"));
        std::env::set_var(key, "Из окружения");
        assert!(env_locked("bot_name"));
        // Пустое значение переменной не считается заданным — так же, как в
        // `apply_env`.
        std::env::set_var(key, "");
        assert!(!env_locked("bot_name"));
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        // У поля без переменной окружения замка не бывает.
        assert!(!env_locked("нет такого ключа"));
    }
}
