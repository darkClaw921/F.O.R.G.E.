//! [`VoiceConfig`] и файл `~/.config/forge/voice.toml`.
//!
//! # ДЕФОЛТЫ ПОДХВАТЫВАЮТСЯ ИЗ `meet.toml`
//!
//! Это главное решение модуля, и оно про человека, а не про код. Ключ polza.ai
//! пользователь уже ввёл — в настройках плагина встреч, где облачная
//! расшифровка появилась первой. Требовать ввести ТОТ ЖЕ ключ второй раз, в
//! другой вкладке, ради той же самой polza.ai — значит, во-первых, гарантированно
//! получить два разошедшихся ключа, во-вторых, завести вторую копию секрета на
//! диске.
//!
//! Порядок разрешения каждого поля:
//!
//! ```text
//! voice.toml (если поле есть и непусто) → meet.toml → встроенный дефолт
//! ```
//!
//! Пустая строка в `voice.toml` — это НЕ «выключить»: файл правят руками, и
//! `polza_api_key = ""` куда чаще означает «не заполнил», чем «хочу без ключа».
//! Явное выключение делается выбором движка, а не пустым полем.
//!
//! # СТАНДАРТ ЧАТОВ — ОБЛАКО, И ДВИЖОК ИЗ `meet.toml` НЕ НАСЛЕДУЕТСЯ
//!
//! Единственное исключение из лестницы выше. Расшифровка голосовых в чатах по
//! умолчанию идёт через облачного провайдера, и `stt_backend = "local"` в
//! настройках встреч этого не меняет.
//!
//! Причина в том, что у встреч и у чатов разные задачи. Час записи встречи
//! локально и бесплатно — осмысленный выбор: ждать её никто не будет всё равно.
//! Голосовое в чате живёт секунды, приходит в любой момент и читается сразу,
//! поэтому оно не должно ни стоять в очереди к GPU, ни требовать 574 МБ весов
//! на машине. Наследуй мы `stt_backend`, настройка одной вкладки молча меняла
//! бы поведение другой — и заметить это можно было бы только по времени ответа.
//!
//! Локальный движок никуда не делся: он включается ЯВНЫМ `engine = "local"` в
//! `voice.toml` (его же пишет селект движка в настройках).
//!
//! # ЗАПИСЬ СОХРАНЯЕТ КОММЕНТАРИИ
//!
//! Через `toml_edit`, как в `plugins/meet/src/config.rs`. Обычная сериализация
//! `toml` переписала бы файл целиком: комментарии и порядок ключей исчезли бы,
//! а вместе с ними — вся документация, которую человек читает прямо в файле,
//! когда правит его руками.
//!
//! # ГДЕ ХРАНИТСЯ КЛЮЧ
//!
//! В [`SecretKey`], который не печатается ни в `Debug`, ни в `Display`, ни в
//! логах. Файл пишется с правами `0600` (через `forge-atomic`), потому что в
//! нём лежит секрет.

use std::path::{Path, PathBuf};

use crate::cloud::{
    SecretKey, DEFAULT_BASE_URL, DEFAULT_CHAT_STT_MODEL, DEFAULT_STT_LANGUAGE,
};
use crate::error::VoiceError;
use crate::hub::{DEFAULT_MAX_PARALLEL, DEFAULT_RUN_TIMEOUT_SECS};
use crate::store::Engine;

/// Переменная, которой переопределяется путь `voice.toml` (нужна тестам).
pub const CONFIG_PATH_ENV: &str = "FORGE_VOICE_CONFIG_PATH";

/// Переменная, которой переопределяется путь `meet.toml`. Имя ТО ЖЕ, что читает
/// сам плагин встреч: два разных пути к одному файлу означали бы, что настройки
/// расходятся ровно в тестах.
pub const MEET_CONFIG_PATH_ENV: &str = "FORGE_MEET_CONFIG_PATH";

/// Потолок длительности голосового сообщения, секунды.
///
/// Пятнадцать минут: длиннее голосовых не бывает ни в одном из трёх
/// мессенджеров, а лимит существует ради чужого ввода — «голосовое» на два часа
/// означает подменённые метаданные, а не разговорчивого собеседника.
pub const DEFAULT_MAX_DURATION_SECS: u64 = 900;

/// Потолок одного облачного запроса, секунды.
pub const DEFAULT_CLOUD_TIMEOUT_SECS: u64 = 120;

/// Модель локального распознавания по умолчанию.
///
/// Та же, что у плагина встреч: файл модели общий, и разные умолчания означали
/// бы вторую копию 574 МБ на диске ради того же самого качества.
pub const DEFAULT_WHISPER_MODEL: &str = "large-v3-turbo-q5_0";

/// Каталог моделей whisper.cpp по умолчанию.
///
/// Тот же, что у `forge-meet` (`~/.forge/meet/models`), и это ГЛАВНОЕ, что
/// здесь можно испортить: свой каталог означал бы, что человек, уже скачавший
/// полтора гигабайта весов во вкладке встреч, качает их второй раз ради
/// голосовых сообщений.
#[must_use]
pub fn default_models_dir() -> PathBuf {
    match std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()) {
        Some(home) => PathBuf::from(home).join(".forge/meet/models"),
        None => PathBuf::from("./models"),
    }
}

/// Настройки голосовых сообщений.
///
/// `Debug` безопасен: ключ внутри — [`SecretKey`], он печатает маску.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConfig {
    /// Каким движком расшифровывать.
    pub engine: Engine,
    /// Язык расшифровки; пустая строка — не передавать движку.
    pub language: String,
    /// Ключ polza.ai.
    pub polza_api_key: SecretKey,
    /// База API облака. Поле, а не константа: это точка подмены в тестах.
    pub cloud_base_url: String,
    /// Модель облачной расшифровки.
    ///
    /// Дефолт — [`DEFAULT_CHAT_STT_MODEL`] (0,048 ₽/мин), а НЕ модель встреч
    /// (0,432 ₽/мин): чатам таймкоды не нужны, значит платить за них незачем.
    pub cloud_stt_model: String,
    /// Потолок длительности голосового, секунды.
    pub max_duration_secs: u64,
    /// Потолок локального прогона whisper, секунды.
    pub local_timeout_secs: u64,
    /// Потолок одного облачного запроса, секунды.
    pub cloud_timeout_secs: u64,
    /// Сколько расшифровок идёт одновременно.
    pub max_parallel: usize,
    /// Явный путь к `ffmpeg`; `None` — искать в `PATH`.
    pub ffmpeg_path: Option<PathBuf>,
    /// Явный путь к бинарю whisper.cpp; `None` — перебор
    /// [`crate::tools::WHISPER_CANDIDATES`].
    pub whisper_path: Option<PathBuf>,
    /// Каталог с весами whisper.cpp. ОБЩИЙ с плагином встреч.
    pub models_dir: PathBuf,
    /// Короткое имя модели распознавания (`large-v3-turbo-q5_0`).
    pub whisper_model: String,
    /// Откуда прочитан конфиг. Нужен сообщениям об ошибках и записи.
    pub path: PathBuf,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            engine: Engine::Cloud,
            language: DEFAULT_STT_LANGUAGE.to_string(),
            polza_api_key: SecretKey::default(),
            cloud_base_url: DEFAULT_BASE_URL.to_string(),
            cloud_stt_model: DEFAULT_CHAT_STT_MODEL.to_string(),
            max_duration_secs: DEFAULT_MAX_DURATION_SECS,
            local_timeout_secs: DEFAULT_RUN_TIMEOUT_SECS,
            cloud_timeout_secs: DEFAULT_CLOUD_TIMEOUT_SECS,
            max_parallel: DEFAULT_MAX_PARALLEL,
            ffmpeg_path: None,
            whisper_path: None,
            models_dir: default_models_dir(),
            whisper_model: DEFAULT_WHISPER_MODEL.to_string(),
            path: default_config_path(),
        }
    }
}

/// Дефолтный путь `~/.config/forge/voice.toml`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    match std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()) {
        Some(home) => PathBuf::from(home).join(".config/forge/voice.toml"),
        None => PathBuf::from("./voice.toml"),
    }
}

/// Путь `voice.toml` с учётом [`CONFIG_PATH_ENV`].
#[must_use]
pub fn config_path() -> PathBuf {
    match std::env::var(CONFIG_PATH_ENV) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => default_config_path(),
    }
}

/// Пути `voice.toml` и `meet.toml` РЯДОМ с конфигом плагина.
///
/// # Зачем ядру знать про чужой конфиг
///
/// Затем, что иначе изоляция тестов держалась бы на переменных окружения.
/// `FORGE_VOICE_CONFIG_PATH` — переменная ПРОЦЕССА: два теста, выставившие её
/// одновременно, читают чужой файл, и это не теория (известная беда
/// `forge-oi1l` в VK). А тест плагина и без того кладёт свой конфиг во
/// временный каталог — значит, привязка «настройки голосовых лежат там же, где
/// настройки плагина» делает изоляцию свойством конструкции, а не дисциплины.
///
/// В бою `anchor` — это `~/.config/forge/<плагин>.toml`, и соседями оказываются
/// ровно те `~/.config/forge/voice.toml` и `~/.config/forge/meet.toml`, которые
/// крейт считает своими по умолчанию: в проде поведение не меняется вовсе.
///
/// Явные переменные окружения сильнее: они существуют ради ручной отладки и
/// «положить конфиг в другое место».
#[must_use]
pub fn paths_next_to(anchor: &Path) -> (PathBuf, PathBuf) {
    let dir = anchor
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    let pick = |env: &str, name: &str, fallback: fn() -> PathBuf| -> PathBuf {
        match std::env::var(env) {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
            _ => match &dir {
                Some(d) => d.join(name),
                None => fallback(),
            },
        }
    };
    (
        pick(CONFIG_PATH_ENV, "voice.toml", config_path),
        pick(MEET_CONFIG_PATH_ENV, "meet.toml", meet_config_path),
    )
}

/// Путь `meet.toml` с учётом [`MEET_CONFIG_PATH_ENV`].
#[must_use]
pub fn meet_config_path() -> PathBuf {
    match std::env::var(MEET_CONFIG_PATH_ENV) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => match std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()) {
            Some(home) => PathBuf::from(home).join(".config/forge/meet.toml"),
            None => PathBuf::from("./meet.toml"),
        },
    }
}

/// Читает TOML-файл. `Ok(None)` — файла нет (это не ошибка).
fn read_table(path: &Path) -> Result<Option<toml::Table>, VoiceError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(VoiceError::Config {
                file: path.display().to_string(),
                reason: e.to_string(),
            })
        }
    };
    raw.parse::<toml::Table>()
        .map(Some)
        .map_err(|e| VoiceError::Config {
            file: path.display().to_string(),
            reason: e.to_string(),
        })
}

/// Непустая строка из таблицы по ключу.
fn str_of(table: Option<&toml::Table>, key: &str) -> Option<String> {
    let v = table?.get(key)?.as_str()?.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Положительное целое из таблицы по ключу.
///
/// Ноль отбрасывается вместе с отрицательными: `local_timeout_secs = 0`
/// означало бы прогон, снимаемый в момент старта, — то есть фичу, выключенную
/// молча опечаткой.
fn uint_of(table: Option<&toml::Table>, key: &str) -> Option<u64> {
    let v = table?.get(key)?.as_integer()?;
    (v > 0).then_some(v as u64)
}

impl VoiceConfig {
    /// Читает конфиг из обычных мест.
    ///
    /// # Errors
    ///
    /// Битый `voice.toml` или `meet.toml` — с указанием, КАКОЙ именно файл
    /// чинить: без имени человек не знает, в каком из двух опечатка.
    pub fn load() -> Result<Self, VoiceError> {
        Self::load_from(&config_path(), &meet_config_path())
    }

    /// То же с явными путями. Единственная точка подмены в тестах.
    pub fn load_from(voice_path: &Path, meet_path: &Path) -> Result<Self, VoiceError> {
        let voice = read_table(voice_path)?;
        let meet = read_table(meet_path)?;
        let (v, m) = (voice.as_ref(), meet.as_ref());

        let d = Self::default();
        // Каждое поле разрешается по одной и той же лестнице:
        // voice.toml → meet.toml → встроенный дефолт.
        let pick = |key: &str, meet_key: &str, fallback: &str| -> String {
            str_of(v, key)
                .or_else(|| str_of(m, meet_key))
                .unwrap_or_else(|| fallback.to_string())
        };
        let pick_uint = |key: &str, meet_key: &str, fallback: u64| -> u64 {
            uint_of(v, key)
                .or_else(|| uint_of(m, meet_key))
                .unwrap_or(fallback)
        };

        // ДВИЖОК — ЕДИНСТВЕННОЕ ПОЛЕ, КОТОРОЕ ИЗ `meet.toml` НЕ НАСЛЕДУЕТСЯ.
        //
        // Остальное (ключ, модель, язык, таймауты) наследуется, чтобы человек
        // не вводил ключ polza.ai дважды. Но `stt_backend = "local"` во встречах
        // — это решение ПРО ВСТРЕЧИ: час записи локально и бесплатно там
        // осмысленнее облака. У чатов стандарт обратный: голосовое живёт
        // секунды, приходит в любой момент и не должно ни ждать очереди к GPU,
        // ни требовать 574 МБ весов на машине. Наследуй мы `stt_backend`,
        // настройка встреч молча уводила бы чаты с облака — и заметить это
        // можно было бы только по времени ответа.
        //
        // Локальный движок остаётся доступен, но ТОЛЬКО явным `engine` в
        // `voice.toml` (его же пишет селект в настройках).
        let engine_raw = str_of(v, "engine");
        let engine = match engine_raw.as_deref().map(str::trim) {
            Some("local") | Some("whisper") => Engine::Local,
            Some("cloud") | Some("polza") => Engine::Cloud,
            Some(other) => {
                return Err(VoiceError::Config {
                    file: voice_path.display().to_string(),
                    reason: format!("engine = {other:?}: допустимы \"local\" и \"cloud\""),
                })
            }
            None => d.engine,
        };

        Ok(Self {
            engine,
            language: pick("language", "whisper_language", &d.language),
            polza_api_key: SecretKey::new(pick("polza_api_key", "polza_api_key", "")),
            cloud_base_url: pick("cloud_base_url", "cloud_base_url", &d.cloud_base_url),
            // Модель НЕ наследуется из meet.toml по той же причине, что и
            // движок: у встреч там стоит дорогая whisper-1 ради таймкодов, и
            // унаследовать её значило бы платить за чаты в девять раз больше
            // ни за что. Своя строка `cloud_stt_model` в voice.toml работает.
            cloud_stt_model: str_of(v, "cloud_stt_model")
                .unwrap_or_else(|| d.cloud_stt_model.clone()),
            max_duration_secs: pick_uint(
                "max_duration_secs",
                "max_duration_secs",
                d.max_duration_secs,
            ),
            local_timeout_secs: pick_uint(
                "local_timeout_secs",
                "whisper_timeout_secs",
                d.local_timeout_secs,
            ),
            cloud_timeout_secs: pick_uint(
                "cloud_timeout_secs",
                "cloud_stt_timeout_secs",
                d.cloud_timeout_secs,
            ),
            max_parallel: pick_uint("max_parallel", "max_parallel", d.max_parallel as u64)
                .max(1) as usize,
            // Пути бинарей и каталог моделей едут по той же лестнице и под теми
            // же именами, что в `meet.toml`: один и тот же ffmpeg на машине не
            // должен требовать двух записей в двух файлах.
            ffmpeg_path: str_of(v, "ffmpeg_path")
                .or_else(|| str_of(m, "ffmpeg_path"))
                .map(PathBuf::from),
            whisper_path: str_of(v, "whisper_path")
                .or_else(|| str_of(m, "whisper_path"))
                .map(PathBuf::from),
            models_dir: str_of(v, "models_dir")
                .or_else(|| str_of(m, "models_dir"))
                .map_or_else(default_models_dir, PathBuf::from),
            whisper_model: pick("whisper_model", "whisper_model", &d.whisper_model),
            path: voice_path.to_path_buf(),
        })
    }

    /// Потолок ОДНОГО прогона для реестра ([`crate::hub::VoiceHub`]).
    ///
    /// Берётся больший из двух потолков движков: реестр один на оба, а
    /// выбранный движок к моменту создания реестра ещё неизвестен — его можно
    /// переключить в настройках, не перезапуская devforge. Меньшее из двух
    /// значений снимало бы локальный прогон посреди работы.
    #[must_use]
    pub fn run_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.local_timeout_secs.max(self.cloud_timeout_secs))
    }

    /// Настроено ли облако. Пустой ключ означает «нет».
    #[must_use]
    pub fn cloud_ready(&self) -> bool {
        !self.polza_api_key.is_empty()
    }

    /// Собирает [`crate::cloud::CloudConfig`] из этих настроек.
    #[must_use]
    pub fn cloud(&self) -> crate::cloud::CloudConfig {
        crate::cloud::CloudConfig {
            base_url: self.cloud_base_url.clone(),
            api_key: self.polza_api_key.clone(),
            stt_model: self.cloud_stt_model.clone(),
            stt_language: self.language.clone(),
            stt_timeout_secs: self.cloud_timeout_secs,
            ..crate::cloud::CloudConfig::default()
        }
    }
}

/// Правит `voice.toml`, СОХРАНЯЯ комментарии и порядок ключей.
///
/// Файла нет — он создаётся с шапкой, объясняющей человеку, что он правит:
/// пустой TOML без единого слова означал бы, что документация фичи живёт только
/// в исходниках.
///
/// # Errors
///
/// Битый файл (его не переписывают вслепую — там могли быть чужие настройки),
/// отказ записи.
pub fn write_fields(path: &Path, fields: &[(&str, toml_edit::Value)]) -> Result<(), VoiceError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(VoiceError::Config {
                file: path.display().to_string(),
                reason: e.to_string(),
            })
        }
    };
    let mut doc = if raw.trim().is_empty() {
        let mut d = toml_edit::DocumentMut::new();
        // Шапка новорождённого файла: её человек увидит первой.
        d.decor_mut().set_prefix(
            "# Настройки голосовых сообщений F.O.R.G.E. (forge-voice).\n\
             #\n\
             # Незаполненные поля берутся из meet.toml — тот же ключ polza.ai\n\
             # вводить второй раз не нужно.\n\n",
        );
        d
    } else {
        raw.parse::<toml_edit::DocumentMut>()
            .map_err(|e| VoiceError::Config {
                file: path.display().to_string(),
                reason: e.to_string(),
            })?
    };

    for (key, value) in fields {
        doc[*key] = toml_edit::Item::Value(value.clone());
    }

    forge_atomic::write_bytes_atomic(path, doc.to_string().as_bytes())
        .map_err(|e| VoiceError::Config {
            file: path.display().to_string(),
            reason: format!("{e}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// **ОБОИХ ФАЙЛОВ НЕТ — ВСТРОЕННЫЕ ДЕФОЛТЫ.**
    #[test]
    fn without_any_file_the_built_in_defaults_win() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VoiceConfig::load_from(&dir.path().join("voice.toml"), &dir.path().join("meet.toml"))
            .unwrap();
        assert_eq!(cfg.engine, Engine::Cloud);
        assert_eq!(cfg.language, DEFAULT_STT_LANGUAGE);
        assert!(cfg.polza_api_key.is_empty());
        assert!(!cfg.cloud_ready());
        assert_eq!(cfg.cloud_stt_model, DEFAULT_CHAT_STT_MODEL);
        assert_eq!(cfg.max_duration_secs, DEFAULT_MAX_DURATION_SECS);
    }

    /// **`voice.toml` НЕТ — ЗНАЧЕНИЯ БЕРУТСЯ ИЗ `meet.toml`.**
    ///
    /// Ключ polza.ai человек уже ввёл там; заставлять вводить его второй раз —
    /// значит гарантированно получить два разошедшихся ключа.
    #[test]
    fn without_voice_toml_the_meet_toml_supplies_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let meet = dir.path().join("meet.toml");
        write(
            &meet,
            r#"
            polza_api_key = "pza_KEYfromMeetTomlLongEnough"
            cloud_stt_model = "openai/whisper-1"
            whisper_language = "en"
            stt_backend = "local"
            whisper_timeout_secs = 1234
            "#,
        );
        let cfg = VoiceConfig::load_from(&dir.path().join("voice.toml"), &meet).unwrap();
        assert_eq!(cfg.polza_api_key.expose(), "pza_KEYfromMeetTomlLongEnough");
        assert!(cfg.cloud_ready());
        assert_eq!(cfg.language, "en");
        assert_eq!(
            cfg.engine,
            Engine::Cloud,
            "движок из meet.toml НЕ наследуется: `stt_backend = \"local\"` — это \
             решение про встречи, а стандарт чатов — облако"
        );
        assert_eq!(cfg.local_timeout_secs, 1234);
    }

    /// **`stt_backend = "local"` ВО ВСТРЕЧАХ НЕ УВОДИТ ЧАТЫ С ОБЛАКА.**
    ///
    /// Отдельный тест рядом с предыдущим потому, что это не деталь наследования,
    /// а стандарт фичи: расшифровка голосовых идёт через облачного провайдера,
    /// пока человек ЯВНО не выбрал локальный движок в `voice.toml`. Тест
    /// краснеет, если кто-нибудь вернёт `stt_backend` в лестницу наследования.
    #[test]
    fn the_meet_toml_backend_never_switches_chats_to_the_local_engine() {
        let dir = tempfile::tempdir().unwrap();
        let meet = dir.path().join("meet.toml");
        let voice = dir.path().join("voice.toml");
        write(&meet, "stt_backend = \"local\"\n");

        // Ни voice.toml вовсе, ни voice.toml без `engine` — оба дают облако.
        let no_voice = VoiceConfig::load_from(&voice, &meet).unwrap();
        assert_eq!(no_voice.engine, Engine::Cloud);

        write(&voice, "language = \"ru\"\n");
        let silent_voice = VoiceConfig::load_from(&voice, &meet).unwrap();
        assert_eq!(silent_voice.engine, Engine::Cloud);

        // Явный выбор человека по-прежнему работает — иначе это был бы запрет,
        // а не стандарт.
        write(&voice, "engine = \"local\"\n");
        let explicit = VoiceConfig::load_from(&voice, &meet).unwrap();
        assert_eq!(explicit.engine, Engine::Local);
    }

    /// **ПОЛЕ, ПЕРЕОПРЕДЕЛЁННОЕ В `voice.toml`, ПОБЕЖДАЕТ.**
    #[test]
    fn a_field_set_in_voice_toml_beats_meet_toml() {
        let dir = tempfile::tempdir().unwrap();
        let meet = dir.path().join("meet.toml");
        let voice = dir.path().join("voice.toml");
        write(
            &meet,
            r#"
            polza_api_key = "pza_KEYfromMeetTomlLongEnough"
            whisper_language = "en"
            stt_backend = "local"
            "#,
        );
        write(
            &voice,
            r#"
            engine = "cloud"
            language = "ru"
            polza_api_key = "pza_KEYfromVoiceTomlLongEnough"
            max_duration_secs = 60
            max_parallel = 3
            "#,
        );
        let cfg = VoiceConfig::load_from(&voice, &meet).unwrap();
        assert_eq!(cfg.polza_api_key.expose(), "pza_KEYfromVoiceTomlLongEnough");
        assert_eq!(cfg.language, "ru");
        assert_eq!(cfg.engine, Engine::Cloud);
        assert_eq!(cfg.max_duration_secs, 60);
        assert_eq!(cfg.max_parallel, 3);
        assert_eq!(cfg.path, voice);
    }

    /// Пустое поле в `voice.toml` — это «не заполнил», а не «выключить».
    #[test]
    fn an_empty_field_falls_through_to_meet_toml() {
        let dir = tempfile::tempdir().unwrap();
        let meet = dir.path().join("meet.toml");
        let voice = dir.path().join("voice.toml");
        write(&meet, r#"polza_api_key = "pza_KEYfromMeetTomlLongEnough""#);
        write(&voice, "polza_api_key = \"\"\nlanguage = \"   \"\n");
        let cfg = VoiceConfig::load_from(&voice, &meet).unwrap();
        assert_eq!(cfg.polza_api_key.expose(), "pza_KEYfromMeetTomlLongEnough");
        assert_eq!(cfg.language, DEFAULT_STT_LANGUAGE);
    }

    /// **БИТЫЙ ФАЙЛ — ВНЯТНАЯ ОШИБКА С ЕГО ИМЕНЕМ, А НЕ ПАНИКА.**
    #[test]
    fn a_broken_file_is_a_named_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        let meet = dir.path().join("meet.toml");
        write(&voice, "это = не = toml");
        let err = VoiceConfig::load_from(&voice, &meet).unwrap_err();
        assert_eq!(err.code(), "voice_config");
        assert!(err.to_string().contains("voice.toml"), "{err}");

        // И `meet.toml` тоже называется по имени: иначе человек не знает,
        // какой из двух файлов чинить.
        write(&voice, "");
        write(&meet, "тоже [не toml");
        let err = VoiceConfig::load_from(&voice, &meet).unwrap_err();
        assert!(err.to_string().contains("meet.toml"), "{err}");

        // Неизвестный движок — тоже отказ, а не молчаливый дефолт: «engine =
        // clould» иначе тихо ушло бы в облако.
        write(&meet, "");
        write(&voice, r#"engine = "clould""#);
        let err = VoiceConfig::load_from(&voice, &meet).unwrap_err();
        assert!(err.to_string().contains("clould"), "{err}");
    }

    /// **КЛЮЧ НЕ ПОПАДАЕТ НИ В `Debug`, НИ В ЛОГИ.**
    #[test]
    fn the_key_never_leaks_through_debug() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        write(&voice, r#"polza_api_key = "pza_TESTnotArealKeyJustAFixture_mask01""#);
        let cfg = VoiceConfig::load_from(&voice, &dir.path().join("meet.toml")).unwrap();
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("notArealKeyJustAFixture"), "ключ утёк: {shown}");
        assert!(shown.contains("pza_TEST"), "{shown}");
        // И собранный из него облачный конфиг — тоже.
        assert!(!format!("{:?}", cfg.cloud()).contains("notArealKeyJustAFixture"));
        assert_eq!(cfg.cloud().api_key.expose(), cfg.polza_api_key.expose());
    }

    /// **ЗАПИСЬ СОХРАНЯЕТ КОММЕНТАРИИ И ПОРЯДОК КЛЮЧЕЙ.**
    ///
    /// Файл правят руками, и комментарии в нём — это и есть его документация.
    #[test]
    fn writing_preserves_comments_and_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        write(
            &voice,
            "# Мой конфиг голосовых.\n\
             # Движок: local или cloud.\n\
             engine = \"local\"\n\
             \n\
             # Язык распознавания.\n\
             language = \"ru\"\n\
             max_duration_secs = 300\n",
        );

        write_fields(
            &voice,
            &[
                ("engine", "cloud".into()),
                ("max_parallel", 2i64.into()),
            ],
        )
        .unwrap();

        let after = std::fs::read_to_string(&voice).unwrap();
        assert!(after.contains("# Мой конфиг голосовых."), "{after}");
        assert!(after.contains("# Язык распознавания."), "{after}");
        assert!(after.contains("engine = \"cloud\""), "{after}");
        assert!(after.contains("max_parallel = 2"), "{after}");
        // Порядок: `engine` по-прежнему ПЕРЕД `language`, новый ключ — в конце.
        let ie = after.find("engine =").unwrap();
        let il = after.find("language =").unwrap();
        let ip = after.find("max_parallel").unwrap();
        assert!(ie < il && il < ip, "порядок ключей поехал: {after}");

        // И прочитанное совпадает с записанным.
        let cfg = VoiceConfig::load_from(&voice, &dir.path().join("meet.toml")).unwrap();
        assert_eq!(cfg.engine, Engine::Cloud);
        assert_eq!(cfg.max_parallel, 2);
        assert_eq!(cfg.max_duration_secs, 300);
    }

    /// Новорождённый файл получает шапку, а не остаётся немым.
    #[test]
    fn a_new_file_is_created_with_a_header() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("nested/voice.toml");
        write_fields(&voice, &[("engine", "cloud".into())]).unwrap();
        let body = std::fs::read_to_string(&voice).unwrap();
        assert!(body.starts_with("# Настройки голосовых"), "{body}");
        assert!(body.contains("meet.toml"), "{body}");
        assert!(body.contains("engine = \"cloud\""), "{body}");
    }

    /// Битый файл не переписывается вслепую: там могли быть чужие настройки.
    #[test]
    fn writing_into_a_broken_file_refuses_instead_of_erasing_it() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        write(&voice, "это = не = toml");
        let err = write_fields(&voice, &[("engine", "cloud".into())]).unwrap_err();
        assert_eq!(err.code(), "voice_config");
        assert_eq!(std::fs::read_to_string(&voice).unwrap(), "это = не = toml");
    }

    /// Нулевые и отрицательные потолки не выключают фичу молча.
    #[test]
    fn zero_limits_fall_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        write(
            &voice,
            "max_duration_secs = 0\nlocal_timeout_secs = -5\nmax_parallel = 0\n",
        );
        let cfg = VoiceConfig::load_from(&voice, &dir.path().join("meet.toml")).unwrap();
        assert_eq!(cfg.max_duration_secs, DEFAULT_MAX_DURATION_SECS);
        assert_eq!(cfg.local_timeout_secs, DEFAULT_RUN_TIMEOUT_SECS);
        assert_eq!(cfg.max_parallel, 1);
    }
}
