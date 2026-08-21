//! Поиск внешних бинарей и файла модели.
//!
//! Крейт запускает ровно два чужих процесса — `ffmpeg` и `whisper-cli` — и оба
//! могут отсутствовать. Модуль отвечает на один вопрос: «есть ли он и где», и
//! отвечает ОТКАЗОМ СО СВОИМ КОДОМ, а не `Option`.
//!
//! # Почему отсутствие бинаря — не `spawn_failed`
//!
//! Технически можно было бы не искать ничего заранее: `tokio::process` всё
//! равно вернёт ошибку запуска. Но `spawn_failed` — это ОДИН код на два разных
//! мира: «ffmpeg не установлен» лечится `brew install ffmpeg`, а «ffmpeg упал
//! при старте» — нет. Фронт по коду решает, какую подсказку показать, и
//! склеивать эти случаи значило бы показывать «установите ffmpeg» человеку, у
//! которого он установлен.
//!
//! Вторая причина — цена: до запуска процесса расшифровка успевает скачать
//! файл из мессенджера. Проверка «бинарь на месте» стоит одного `stat` и стоит
//! ПЕРЕД скачиванием.
//!
//! # Свой обход `PATH`, а не крейт `which`
//!
//! Ровно та же причина, что в `plugins/meet/src/deps.rs`, откуда правило и
//! взято: нужен один вызов, а новый узел в дереве зависимостей стоит дороже
//! пятнадцати строк. Правило поиска обязано СОВПАДАТЬ с meet — иначе плагин
//! встреч и голосовые сообщения находили бы разные бинари на одной машине.

use std::path::{Path, PathBuf};

use crate::error::VoiceError;

/// Имя ffmpeg в `PATH`.
pub const FFMPEG_NAME: &str = "ffmpeg";

/// Кандидаты на роль whisper.cpp, в порядке предпочтения.
///
/// Список дословно повторяет `plugins/meet/src/deps.rs::WHISPER_CANDIDATES`:
/// формула brew ставит `whisper-cli`, сборка из исходников — `main`, а под
/// именем `whisper` в `PATH` часто лежит Python-овский openai-whisper, у
/// которого расчёт по умолчанию идёт НА ПРОЦЕССОРЕ. Последний кандидат поэтому
/// и последний.
pub const WHISPER_CANDIDATES: [&str; 4] = ["whisper-cli", "whisper-cpp", "main", "whisper"];

/// Префикс имени файла модели whisper.cpp.
pub const MODEL_FILE_PREFIX: &str = "ggml-";

/// Расширение файла модели.
pub const MODEL_FILE_SUFFIX: &str = ".bin";

/// Ищет исполняемый файл в `PATH`.
///
/// Имя с разделителем каталогов считается готовым путём и в `PATH` не ищется —
/// иначе `./my-ffmpeg` резолвился бы в системный.
#[must_use]
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Путь ffmpeg: явный из конфига либо найденный в `PATH`.
///
/// # Errors
///
/// [`VoiceError::MissingBinary`] с кодом `voice_no_ffmpeg`. Явно заданный, но
/// несуществующий путь — тоже отказ, и в тексте называется ИМЕННО ОН: молча
/// уходить на системный ffmpeg вместо указанного значило бы игнорировать
/// настройку человека.
pub fn resolve_ffmpeg(explicit: Option<&Path>) -> Result<PathBuf, VoiceError> {
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(VoiceError::MissingBinary {
                what: FFMPEG_NAME,
                hint: format!(
                    "в настройках указан {}, но такого файла нет",
                    p.display()
                ),
            })
        };
    }
    find_in_path(FFMPEG_NAME).ok_or_else(|| VoiceError::MissingBinary {
        what: FFMPEG_NAME,
        hint: "не найден в PATH; установите ffmpeg (brew install ffmpeg) \
               или укажите ffmpeg_path в voice.toml"
            .to_string(),
    })
}

/// Путь whisper.cpp: явный из конфига либо первый найденный кандидат.
///
/// # Errors
///
/// [`VoiceError::MissingBinary`] с кодом `voice_no_whisper`.
pub fn resolve_whisper(explicit: Option<&Path>) -> Result<PathBuf, VoiceError> {
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(VoiceError::MissingBinary {
                what: "whisper",
                hint: format!("в настройках указан {}, но такого файла нет", p.display()),
            })
        };
    }
    WHISPER_CANDIDATES
        .iter()
        .find_map(|name| find_in_path(name))
        .ok_or_else(|| VoiceError::MissingBinary {
            what: "whisper",
            hint: format!(
                "ни один из {} не найден в PATH; поставьте whisper.cpp \
                 (brew install whisper-cpp) либо выберите облачный движок",
                WHISPER_CANDIDATES.join(", ")
            ),
        })
}

/// Имя файла модели по её короткому имени.
#[must_use]
pub fn model_file_name(name: &str) -> String {
    format!("{MODEL_FILE_PREFIX}{name}{MODEL_FILE_SUFFIX}")
}

/// Путь файла модели в каталоге моделей.
#[must_use]
pub fn model_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(model_file_name(name))
}

/// Проверяет, что модель на диске есть и она не пустая.
///
/// Пустой файл моделью не считается: так выглядит прерванное копирование и
/// неудачный `touch`, а whisper на таком входе падает с невнятным стеком ggml.
///
/// # Errors
///
/// [`VoiceError::MissingModel`] с кодом `voice_no_model` — с полным путём,
/// чтобы человеку было куда положить файл.
pub fn require_model(models_dir: &Path, name: &str) -> Result<PathBuf, VoiceError> {
    let path = model_path(models_dir, name);
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() && m.len() > 0 => Ok(path),
        Ok(_) => Err(VoiceError::MissingModel(format!(
            "{} пуст или не файл",
            path.display()
        ))),
        Err(_) => Err(VoiceError::MissingModel(format!(
            "{} нет на диске; скачайте модель во вкладке встреч либо \
             выберите облачный движок",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пустой `PATH` — это отказ с кодом «нет ffmpeg», а не паника и не
    /// молчаливое «спишем на потом».
    #[test]
    fn a_missing_binary_is_a_named_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("нет-такого");

        let err = resolve_ffmpeg(Some(&fake)).unwrap_err();
        assert_eq!(err.code(), "voice_no_ffmpeg");
        assert!(err.to_string().contains("нет-такого"), "{err}");

        let err = resolve_whisper(Some(&fake)).unwrap_err();
        assert_eq!(err.code(), "voice_no_whisper");
    }

    /// Явно указанный существующий файл побеждает `PATH`.
    #[test]
    fn an_explicit_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("ffmpeg");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        assert_eq!(resolve_ffmpeg(Some(&bin)).unwrap(), bin);
        assert_eq!(resolve_whisper(Some(&bin)).unwrap(), bin);
    }

    /// Путь с разделителем каталогов в `PATH` не ищется.
    #[test]
    fn a_relative_name_is_taken_as_a_path() {
        assert!(find_in_path("./заведомо-нет").is_none());
        assert!(find_in_path("/bin/sh").is_some(), "/bin/sh есть на любой unix-машине");
    }

    /// **МОДЕЛИ НЕТ ИЛИ ОНА ПУСТА — ОТКАЗ `voice_no_model` С ПУТЁМ.**
    #[test]
    fn a_missing_or_empty_model_is_refused_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = require_model(dir.path(), "large-v3-turbo-q5_0").unwrap_err();
        assert_eq!(err.code(), "voice_no_model");
        assert!(err.to_string().contains("ggml-large-v3-turbo-q5_0.bin"), "{err}");

        // Пустой файл моделью не считается.
        let path = model_path(dir.path(), "tiny");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(require_model(dir.path(), "tiny").unwrap_err().code(), "voice_no_model");

        std::fs::write(&path, b"lmgg____").unwrap();
        assert_eq!(require_model(dir.path(), "tiny").unwrap(), path);
    }

    /// Имя файла модели строится так же, как в плагине встреч: иначе один и тот
    /// же файл на диске считался бы отсутствующим у одной из двух половин.
    #[test]
    fn the_model_file_name_matches_the_meet_layout() {
        assert_eq!(model_file_name("large-v3-turbo-q5_0"), "ggml-large-v3-turbo-q5_0.bin");
    }
}
