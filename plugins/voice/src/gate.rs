//! Единая лестница проверок перед запуском расшифровки.
//!
//! Три плагина задают один и тот же вопрос — «можно ли расшифровать вот это
//! вложение прямо сейчас» — и обязаны получать один и тот же ответ. Поэтому
//! лестница живёт ЗДЕСЬ, а не тремя копиями в трёх роутах: разошедшиеся копии
//! означали бы, что в VK «слишком длинное» наступает на пятнадцатой минуте, а
//! в Bitrix на десятой, причём выяснилось бы это через полгода на живой
//! записи.
//!
//! # ПОРЯДОК СТУПЕНЕЙ
//!
//! ```text
//! вид вложения ──▶ длительность ──▶ движок ──▶ бинари и модель ──▶ ключ облака
//!    (400)            (400)                       (409)              (409)
//! ```
//!
//! Порядок не случаен: сначала отказы, зависящие ТОЛЬКО от запроса (не
//! голосовое, слишком длинное), потом — зависящие от состояния машины (нет
//! ffmpeg, нет модели, нет ключа). Иначе человек с ненастроенным облаком,
//! ткнувший «расшифровать» в фотографию, узнал бы про отсутствующий ключ
//! вместо того, что он ткнул не туда.
//!
//! Проверка потолка параллелизма в лестницу НЕ входит: она живёт в
//! [`crate::hub::VoiceHub::start`] и делается ПОД ЗАМКОМ вместе с занятием
//! слота — вынести её сюда значило бы завести окно между «посмотрел» и
//! «занял».
//!
//! # ДЛИТЕЛЬНОСТЬ ПРОВЕРЯЕТСЯ ДВАЖДЫ, И ЭТО НАМЕРЕННО
//!
//! Здесь — по метаданным мессенджера, если он их прислал (VK и Telegram
//! присылают, Bitrix — нет). Второй раз — в [`crate::audio::prepare`], уже по
//! фактически декодированному звуку. Первая проверка бесплатная и отсекает до
//! скачивания; вторая — единственная, которой можно верить, потому что
//! длительность в метаданных задаёт отправитель.
//!
//! # ЛОКАЛЬНЫЙ ДВИЖОК НЕ ПАДАЕТ НА ПРОЦЕССОР
//!
//! Правило проекта: локальные модели считаются только на GPU. Лестница
//! проверяет наличие бинаря и весов, а сам Metal подтверждается уже прогоном
//! ([`crate::whisper`], `metal_unavailable`) — заранее это не проверить, и
//! молчаливого отката на облако тут НЕТ: подмена движка означала бы, что
//! голосовое ушло в чужое облако после того, как человек выбрал локальный
//! расчёт.

use std::path::PathBuf;

use crate::config::VoiceConfig;
use crate::error::VoiceError;
use crate::store::Engine;
use crate::tools;

/// Виды вложений, которые имеет смысл расшифровывать.
///
/// Списком, а не «всё, что похоже на аудио»: `kind` приходит из плагина, и три
/// мессенджера называют вещи по-разному (`voice` у VK и Telegram,
/// `audio_message` в старых DTO, `voice`/`audio` у Bitrix). Музыкальный трек
/// (`audio`) в списке ЕСТЬ намеренно: пересланная запись разговора приходит
/// именно им, и запрещать её значило бы решать за человека.
pub const VOICE_KINDS: [&str; 5] = ["voice", "audio", "voice_message", "audio_message", "music"];

/// Годится ли вид вложения для расшифровки.
#[must_use]
pub fn is_voice_kind(kind: &str) -> bool {
    let k = kind.trim().to_ascii_lowercase();
    VOICE_KINDS.contains(&k.as_str())
}

/// Что плагин знает о вложении к моменту проверки.
#[derive(Debug, Clone, Default)]
pub struct VoiceRequest {
    /// Вид вложения, как его назвал плагин (`voice`, `photo`, …).
    pub kind: String,
    /// Длительность из метаданных мессенджера; `None` — он её не прислал
    /// (Bitrix не присылает никогда).
    pub duration_secs: Option<i64>,
}

impl VoiceRequest {
    /// Запрос с одним лишь видом вложения.
    #[must_use]
    pub fn of_kind(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            duration_secs: None,
        }
    }

    /// Тот же запрос с известной длительностью.
    #[must_use]
    pub fn with_duration_secs(mut self, secs: Option<i64>) -> Self {
        self.duration_secs = secs;
        self
    }
}

/// Чем и как расшифровывать — итог лестницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePlan {
    /// Выбранный движок.
    pub engine: Engine,
    /// Путь ffmpeg. Нужен ОБОИМ движкам: без перекодирования не обходится ни
    /// облако (mp3), ни whisper (WAV 16 кГц).
    pub ffmpeg: PathBuf,
    /// Бинарь whisper.cpp — только у локального движка.
    pub whisper: Option<PathBuf>,
    /// Файл весов — только у локального движка.
    pub model: Option<PathBuf>,
}

impl EnginePlan {
    /// Имя движка для кадра `started` и для стора.
    #[must_use]
    pub fn engine_name(&self) -> &'static str {
        self.engine.as_str()
    }
}

/// Прогоняет лестницу и говорит, чем расшифровывать.
///
/// # Errors
///
/// По одному коду на ступень — их различает и фронт, и тесты:
///
/// | код | причина |
/// |---|---|
/// | `voice_not_audio` | вложение не голосовое и не аудио |
/// | `voice_too_long` | длиннее потолка `max_duration_secs` |
/// | `voice_no_ffmpeg` | ffmpeg не найден |
/// | `voice_no_whisper` | локальный движок, а бинаря whisper.cpp нет |
/// | `voice_no_model` | локальный движок, а весов на диске нет |
/// | `voice_no_key` | облачный движок, а ключ polza.ai не задан |
pub fn check(cfg: &VoiceConfig, req: &VoiceRequest) -> Result<EnginePlan, VoiceError> {
    if !is_voice_kind(&req.kind) {
        return Err(VoiceError::NotVoice(if req.kind.trim().is_empty() {
            "без вложения".to_string()
        } else {
            req.kind.trim().to_string()
        }));
    }

    if let Some(secs) = req.duration_secs {
        if cfg.max_duration_secs > 0 && secs > cfg.max_duration_secs as i64 {
            return Err(VoiceError::TooLong {
                secs,
                limit: cfg.max_duration_secs,
            });
        }
    }

    let ffmpeg = tools::resolve_ffmpeg(cfg.ffmpeg_path.as_deref())?;

    match cfg.engine {
        Engine::Local => {
            let whisper = tools::resolve_whisper(cfg.whisper_path.as_deref())?;
            let model = tools::require_model(&cfg.models_dir, &cfg.whisper_model)?;
            Ok(EnginePlan {
                engine: Engine::Local,
                ffmpeg,
                whisper: Some(whisper),
                model: Some(model),
            })
        }
        Engine::Cloud => {
            if !cfg.cloud_ready() {
                return Err(VoiceError::NoCloudKey);
            }
            Ok(EnginePlan {
                engine: Engine::Cloud,
                ffmpeg,
                whisper: None,
                model: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::SecretKey;

    /// Конфиг, у которого ВСЁ на месте: дальше тесты ломают по одной вещи.
    fn ready(dir: &std::path::Path) -> VoiceConfig {
        let ffmpeg = dir.join("ffmpeg");
        std::fs::write(&ffmpeg, b"#!/bin/sh\n").unwrap();
        let whisper = dir.join("whisper-cli");
        std::fs::write(&whisper, b"#!/bin/sh\n").unwrap();
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-tiny.bin"), b"lmgg____").unwrap();

        VoiceConfig {
            engine: Engine::Cloud,
            polza_api_key: SecretKey::new("pza_TESTnotArealKeyJustAFixture"),
            ffmpeg_path: Some(ffmpeg),
            whisper_path: Some(whisper),
            models_dir: models,
            whisper_model: "tiny".to_string(),
            ..VoiceConfig::default()
        }
    }

    /// **НЕ ГОЛОСОВОЕ — ОТКАЗ ПЕРВОЙ СТУПЕНЬЮ, И НАЗЫВАЕТ ВИД ВЛОЖЕНИЯ.**
    #[test]
    fn a_non_voice_attachment_is_refused_first() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ready(dir.path());
        for kind in ["photo", "video", "document", "sticker", "", "   "] {
            let err = check(&cfg, &VoiceRequest::of_kind(kind)).unwrap_err();
            assert_eq!(err.code(), "voice_not_audio", "{kind:?}");
        }
        assert!(check(&cfg, &VoiceRequest::of_kind("photo"))
            .unwrap_err()
            .to_string()
            .contains("photo"));

        // А голосовые и аудио проходят — во всех написаниях трёх плагинов.
        for kind in ["voice", "audio", "VOICE", " voice_message ", "music"] {
            assert!(check(&cfg, &VoiceRequest::of_kind(kind)).is_ok(), "{kind:?}");
        }
    }

    /// **СЛИШКОМ ДЛИННОЕ — ОТКАЗ С ПОТОЛКОМ В ТЕКСТЕ.**
    ///
    /// И проверяется ДО состояния машины: сломанный конфиг не должен
    /// перекрывать ошибку в самом запросе.
    #[test]
    fn a_too_long_recording_names_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ready(dir.path());
        cfg.max_duration_secs = 60;

        let err = check(
            &cfg,
            &VoiceRequest::of_kind("voice").with_duration_secs(Some(61)),
        )
        .unwrap_err();
        assert_eq!(err.code(), "voice_too_long");
        assert!(err.to_string().contains("60"), "{err}");
        assert!(err.to_string().contains("61"), "{err}");

        // Ровно потолок — ещё можно.
        assert!(check(
            &cfg,
            &VoiceRequest::of_kind("voice").with_duration_secs(Some(60))
        )
        .is_ok());
        // Неизвестная длительность (Bitrix) ступень не заваливает: её проверит
        // подготовка аудио по фактически декодированному звуку.
        assert!(check(&cfg, &VoiceRequest::of_kind("voice")).is_ok());

        // И длительность важнее отсутствующего ключа.
        cfg.polza_api_key = SecretKey::default();
        assert_eq!(
            check(&cfg, &VoiceRequest::of_kind("voice").with_duration_secs(Some(99)))
                .unwrap_err()
                .code(),
            "voice_too_long"
        );
    }

    /// **НЕТ ffmpeg — СВОЙ КОД, У ОБОИХ ДВИЖКОВ.**
    #[test]
    fn a_missing_ffmpeg_refuses_both_engines() {
        let dir = tempfile::tempdir().unwrap();
        for engine in [Engine::Cloud, Engine::Local] {
            let mut cfg = ready(dir.path());
            cfg.engine = engine;
            cfg.ffmpeg_path = Some(dir.path().join("нет-такого"));
            let err = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err();
            assert_eq!(err.code(), "voice_no_ffmpeg", "{engine:?}");
        }
    }

    /// **ЛОКАЛЬНЫЙ ДВИЖОК: НЕТ БИНАРЯ И НЕТ МОДЕЛИ — ДВА РАЗНЫХ КОДА.**
    ///
    /// Разные потому, что разное лечение: бинарь ставится пакетным менеджером,
    /// а веса скачиваются. Общий `voice_local_unavailable` отправил бы половину
    /// пользователей чинить не то.
    #[test]
    fn the_local_engine_distinguishes_a_missing_binary_from_missing_weights() {
        let dir = tempfile::tempdir().unwrap();

        let mut cfg = ready(dir.path());
        cfg.engine = Engine::Local;
        cfg.whisper_path = Some(dir.path().join("нет-такого"));
        assert_eq!(
            check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err().code(),
            "voice_no_whisper"
        );

        let mut cfg = ready(dir.path());
        cfg.engine = Engine::Local;
        cfg.whisper_model = "нет-такой-модели".into();
        let err = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err();
        assert_eq!(err.code(), "voice_no_model");
        assert!(err.to_string().contains("ggml-нет-такой-модели.bin"), "{err}");
    }

    /// **ОБЛАКО БЕЗ КЛЮЧА — ОТКАЗ, И В ТЕКСТЕ НЕТ НИ КЛЮЧА, НИ ЕГО МАСКИ.**
    #[test]
    fn the_cloud_engine_without_a_key_is_refused_without_leaking_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ready(dir.path());
        cfg.polza_api_key = SecretKey::default();
        let err = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err();
        assert_eq!(err.code(), "voice_no_key");

        // Инвариант утечки: ни один текст лестницы не содержит ключа.
        let mut cfg = ready(dir.path());
        cfg.polza_api_key = SecretKey::new("pza_SECRET-CANARY-0123456789");
        cfg.ffmpeg_path = Some(dir.path().join("нет"));
        let err = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err();
        assert!(!err.to_string().contains("SECRET-CANARY"), "{err}");
    }

    /// Успешная лестница отдаёт полный план: у локального движка есть и бинарь,
    /// и веса, у облачного — ни того, ни другого.
    #[test]
    fn a_successful_check_returns_a_complete_plan() {
        let dir = tempfile::tempdir().unwrap();

        let cfg = ready(dir.path());
        let plan = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap();
        assert_eq!(plan.engine, Engine::Cloud);
        assert_eq!(plan.engine_name(), "cloud");
        assert!(plan.whisper.is_none() && plan.model.is_none());
        assert!(plan.ffmpeg.is_file());

        let mut cfg = ready(dir.path());
        cfg.engine = Engine::Local;
        let plan = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap();
        assert_eq!(plan.engine_name(), "local");
        assert!(plan.whisper.unwrap().is_file());
        assert!(plan.model.unwrap().ends_with("ggml-tiny.bin"));
    }

    /// **МОЛЧАЛИВОЙ ПОДМЕНЫ ДВИЖКА НЕТ.**
    ///
    /// Локальный движок без модели отвечает отказом, а НЕ уезжает в облако,
    /// даже когда ключ облака на месте. Обратное означало бы, что голосовое
    /// ушло чужому сервису после того, как человек выбрал считать дома.
    #[test]
    fn a_broken_local_engine_never_falls_back_to_the_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ready(dir.path());
        cfg.engine = Engine::Local;
        cfg.whisper_model = "нет".into();
        assert!(cfg.cloud_ready(), "ключ облака на месте — соблазн максимален");

        let err = check(&cfg, &VoiceRequest::of_kind("voice")).unwrap_err();
        assert_eq!(err.code(), "voice_no_model");
    }

    /// Класс отказа одинаков для всех трёх плагинов — на нём держится
    /// одинаковый HTTP-код.
    #[test]
    fn every_refusal_has_a_stable_class() {
        use crate::error::RefusalClass;
        let cases = [
            (VoiceError::NotVoice("photo".into()), RefusalClass::Input),
            (VoiceError::TooLong { secs: 10, limit: 5 }, RefusalClass::Input),
            (
                VoiceError::MissingBinary {
                    what: "ffmpeg",
                    hint: String::new(),
                },
                RefusalClass::State,
            ),
            (VoiceError::MissingModel("x".into()), RefusalClass::State),
            (VoiceError::NoCloudKey, RefusalClass::State),
            (VoiceError::Busy(2), RefusalClass::State),
            (VoiceError::Cancelled, RefusalClass::Unavailable),
            (VoiceError::Audio("x".into()), RefusalClass::Unavailable),
            (VoiceError::Storage("x".into()), RefusalClass::Internal),
        ];
        for (err, class) in cases {
            assert_eq!(err.class(), class, "{err}");
        }
    }
}
