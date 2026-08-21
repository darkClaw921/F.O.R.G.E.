//! Настройки расшифровки, которые человек меняет из браузера.
//!
//! Сегодня это ровно одно значение — ДВИЖОК (`local` / `cloud`). Живёт оно в
//! `voice.toml`, а не в `user_settings.json`, и это осознанно:
//!
//! - движок ОДИН на все мессенджеры (разбирать голосовые VK локально, а
//!   телеграмные — в облаке, никто не просил, зато «почему в одной вкладке
//!   работает, а в другой нет» спросил бы каждый);
//! - рядом, в том же файле, лежит ключ polza.ai, а секретам в
//!   `user_settings.json` не место: тот файл фронт выгребает целиком через
//!   `GET /api/user-settings`.
//!
//! # КЛЮЧ НАРУЖУ НЕ ОТДАЁТСЯ НИКОГДА
//!
//! Ни целиком, ни маской, ни длиной. Ответ несёт лишь признак
//! [`VoiceSettings::cloud_key_set`] — «ключ задан». Этого хватает, чтобы UI
//! написал «введите ключ в voice.toml», и не хватает ни на что другое.
//!
//! # ВЫБОР СОХРАНЯЕТСЯ, ДАЖЕ ЕСЛИ ОН СЕЙЧАС НЕРАБОЧИЙ
//!
//! Человек выбирает `local` на машине без весов — мы записываем выбор и
//! ВОЗВРАЩАЕМ ПРЕДУПРЕЖДЕНИЕ ([`VoiceSettings::warning`]), а не отказ. Отказ
//! означал бы порядок «сначала скачай полтора гигабайта модели вслепую, потом
//! переключай», хотя настоящий порядок обратный: человек переключает движок и
//! читает, чего не хватает. Молчаливо оставаться на прежнем движке нельзя тем
//! более: следующее голосовое ушло бы в облако вопреки только что сделанному
//! выбору.
//!
//! Запись идёт через [`crate::config::write_fields`], то есть `toml_edit` +
//! `forge-atomic`: комментарии и порядок ключей в файле переживают правку, а
//! оборванная посреди записи машина оставляет либо старый файл, либо новый.

use std::path::Path;

use crate::config::{self, VoiceConfig};
use crate::error::VoiceError;
use crate::gate::{self, VoiceRequest};
use crate::store::Engine;

/// Ключ движка в `voice.toml`.
pub const ENGINE_KEY: &str = "engine";

/// Предупреждение «выбор сохранён, но работать не будет».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Машинный код отказа ядра (`voice_no_key`, `voice_no_model`, …).
    pub code: &'static str,
    /// Человеческий текст — его показывает UI.
    pub message: String,
}

/// Состояние настроек расшифровки, каким его видит браузер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSettings {
    /// Выбранный движок.
    pub engine: Engine,
    /// Задан ли ключ polza.ai. САМ КЛЮЧ НАРУЖУ НЕ ХОДИТ.
    pub cloud_key_set: bool,
    /// Чего не хватает, чтобы выбранный движок заработал. `None` — всё на
    /// месте.
    pub warning: Option<Warning>,
}

/// Разбирает слово движка из запроса.
///
/// Принимаются те же написания, что и в `voice.toml`
/// ([`crate::config::VoiceConfig::load_from`]): человек, правивший файл
/// руками, и человек, тыкающий в селект, не должны получать разные словари.
///
/// # Errors
///
/// [`VoiceError::NotVoice`]… нет: неизвестное слово — это
/// [`VoiceError::Config`] с классом «ошибка ввода»? Тоже нет. Здесь
/// возвращается `None`, а HTTP-код выбирает плагин: ядро про `400` не знает.
#[must_use]
pub fn parse_engine(raw: &str) -> Option<Engine> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "local" | "whisper" => Some(Engine::Local),
        "cloud" | "polza" => Some(Engine::Cloud),
        _ => None,
    }
}

/// Собирает состояние настроек по прочитанному конфигу.
///
/// Лестница проверок гоняется с заведомо голосовым видом и без длительности —
/// нас интересуют только ступени «состояние машины»: бинари, модель, ключ.
#[must_use]
pub fn describe(cfg: &VoiceConfig) -> VoiceSettings {
    let warning = match gate::check(cfg, &VoiceRequest::of_kind("voice")) {
        Ok(_) => None,
        Err(e) => Some(Warning {
            code: e.code(),
            message: e.to_string(),
        }),
    };
    VoiceSettings {
        engine: cfg.engine,
        cloud_key_set: cfg.cloud_ready(),
        warning,
    }
}

/// Записывает выбранный движок в `voice.toml` и отдаёт новое состояние.
///
/// Конфиг ПЕРЕЧИТЫВАЕТСЯ после записи, а не правится в памяти: ответ обязан
/// показывать то, что теперь лежит на диске, — иначе опечатка в соседнем ключе
/// (файл правят и руками) всплыла бы только при следующем голосовом.
///
/// # Errors
///
/// [`VoiceError::Config`] — битый файл (его не переписывают вслепую: там могли
/// быть чужие настройки) либо отказ записи.
pub fn set_engine(
    voice_path: &Path,
    meet_path: &Path,
    engine: Engine,
) -> Result<VoiceSettings, VoiceError> {
    config::write_fields(
        voice_path,
        &[(ENGINE_KEY, toml_edit::Value::from(engine.as_str()))],
    )?;
    let cfg = VoiceConfig::load_from(voice_path, meet_path)?;
    Ok(describe(&cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::SecretKey;

    /// Конфиг, у которого на месте ВСЁ для облака.
    fn cloud_ready(dir: &Path) -> VoiceConfig {
        let ffmpeg = dir.join("ffmpeg");
        std::fs::write(&ffmpeg, b"#!/bin/sh\n").unwrap();
        VoiceConfig {
            engine: Engine::Cloud,
            polza_api_key: SecretKey::new("pza_TESTnotArealKeyJustAFixture"),
            ffmpeg_path: Some(ffmpeg),
            ..VoiceConfig::default()
        }
    }

    /// Словарь движка тот же, что у `voice.toml`, а мусор отвергается.
    #[test]
    fn the_engine_word_is_parsed_like_the_config_does() {
        assert_eq!(parse_engine("local"), Some(Engine::Local));
        assert_eq!(parse_engine(" WHISPER "), Some(Engine::Local));
        assert_eq!(parse_engine("cloud"), Some(Engine::Cloud));
        assert_eq!(parse_engine("polza"), Some(Engine::Cloud));
        for bad in ["", "  ", "gpu", "openai", "local2"] {
            assert_eq!(parse_engine(bad), None, "{bad:?}");
        }
    }

    /// **ВСЁ НА МЕСТЕ — ПРЕДУПРЕЖДЕНИЯ НЕТ, КЛЮЧ НЕ СВЕТИТСЯ.**
    #[test]
    fn a_working_setup_has_no_warning_and_never_shows_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = describe(&cloud_ready(dir.path()));
        assert_eq!(s.engine, Engine::Cloud);
        assert!(s.cloud_key_set);
        assert!(s.warning.is_none());
        assert!(!format!("{s:?}").contains("pza_"), "{s:?}");
    }

    /// **ОБЛАКО БЕЗ КЛЮЧА И ЛОКАЛЬНЫЙ БЕЗ МОДЕЛИ — ПРЕДУПРЕЖДЕНИЕ СО СВОИМ
    /// КОДОМ.**
    #[test]
    fn a_broken_setup_warns_with_a_machine_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cloud_ready(dir.path());
        cfg.polza_api_key = SecretKey::default();
        let s = describe(&cfg);
        assert!(!s.cloud_key_set);
        assert_eq!(s.warning.as_ref().unwrap().code, "voice_no_key");

        let mut cfg = cloud_ready(dir.path());
        cfg.engine = Engine::Local;
        cfg.whisper_path = Some(dir.path().join("nonexistent-whisper"));
        let s = describe(&cfg);
        assert_eq!(s.engine, Engine::Local);
        assert_eq!(s.warning.as_ref().unwrap().code, "voice_no_whisper");
        assert!(!s.warning.unwrap().message.is_empty());
    }

    /// **ЗАПИСЬ СОХРАНЯЕТ КОММЕНТАРИИ И ЧУЖИЕ КЛЮЧИ.**
    ///
    /// Файл человек правит руками, и переписать его целиком значило бы стереть
    /// объяснения, которые он там читает.
    #[test]
    fn writing_the_engine_keeps_comments_and_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        let meet = dir.path().join("meet.toml");
        std::fs::write(
            &voice,
            "# мой любимый комментарий\nengine = \"cloud\"\n\
             polza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n\
             # и второй\nmax_duration_secs = 300\n",
        )
        .unwrap();
        std::fs::write(&meet, b"").unwrap();

        let s = set_engine(&voice, &meet, Engine::Local).unwrap();
        assert_eq!(s.engine, Engine::Local);

        let raw = std::fs::read_to_string(&voice).unwrap();
        assert!(raw.contains("# мой любимый комментарий"), "{raw}");
        assert!(raw.contains("# и второй"), "{raw}");
        assert!(raw.contains("max_duration_secs = 300"), "{raw}");
        assert!(raw.contains("engine = \"local\""), "{raw}");
        assert!(raw.contains("pza_TESTnotArealKeyJustAFixture"), "ключ не потерян");
        // Ровно один ключ движка: правка, а не дописывание.
        assert_eq!(raw.matches("engine =").count(), 1, "{raw}");
    }

    /// **ФАЙЛА НЕ БЫЛО — ОН РОЖДАЕТСЯ, И ВЫБОР ПЕРЕЖИВАЕТ ПЕРЕЧИТЫВАНИЕ.**
    #[test]
    fn a_missing_file_is_created_and_the_choice_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        let meet = dir.path().join("meet.toml");
        std::fs::write(&meet, b"").unwrap();

        set_engine(&voice, &meet, Engine::Local).unwrap();
        assert_eq!(
            VoiceConfig::load_from(&voice, &meet).unwrap().engine,
            Engine::Local
        );
        set_engine(&voice, &meet, Engine::Cloud).unwrap();
        assert_eq!(
            VoiceConfig::load_from(&voice, &meet).unwrap().engine,
            Engine::Cloud
        );
    }

    /// **БИТЫЙ ФАЙЛ НЕ ПЕРЕПИСЫВАЕТСЯ ВСЛЕПУЮ.**
    #[test]
    fn a_broken_file_is_refused_instead_of_being_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.toml");
        let meet = dir.path().join("meet.toml");
        let body = "eto = ne = toml\n";
        std::fs::write(&voice, body).unwrap();
        std::fs::write(&meet, b"").unwrap();

        let err = set_engine(&voice, &meet, Engine::Local).unwrap_err();
        assert_eq!(err.code(), "voice_config");
        assert_eq!(std::fs::read_to_string(&voice).unwrap(), body, "файл не тронут");
    }
}
