//! Ошибки ядра голосовых сообщений.
//!
//! Крейт про HTTP не знает вовсе: он не выбирает код ответа и не строит конверт
//! ошибки — это работа плагина, у которого своя таблица кодов. Наружу отдаётся
//! ЗАКРЫТЫЙ список причин, по которому плагин делает `match`, а не разбор строк.
//!
//! # Почему рядом живут два типа ошибок
//!
//! [`VoiceError`] — ошибка ЯДРА (стор, конфиг, огибающая). Отказ ВНЕШНЕГО
//! ПРОЦЕССА описывает [`crate::proc::ProcError`], а отказ облака —
//! [`crate::cloud::CloudError`]: у обоих есть то, чего у обычной ошибки нет и
//! не должно быть — машиночитаемый код (`metal_unavailable`, `cloud_no_funds`)
//! и хвост stderr. Оба заворачиваются сюда вариантами, но НЕ растворяются в
//! строке: `code()` доживает до вызывающего.

use crate::cloud::CloudError;
use crate::proc::ProcError;

/// Почему расшифровка (или подготовка к ней) не удалась.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoiceError {
    /// Отказ внешнего процесса: whisper, ffmpeg. Несёт свой код и хвост stderr.
    #[error("{0}")]
    Proc(#[from] ProcError),

    /// Отказ облачного вызова polza.ai.
    #[error("{0}")]
    Cloud(#[from] CloudError),

    /// Файла с аудио нет, он пуст или не читается.
    #[error("аудио недоступно: {0}")]
    Audio(String),

    /// Хранилище транскриптов не прочиталось/не записалось. Внутри — уже
    /// обезличенное описание причины.
    #[error("хранилище недоступно: {0}")]
    Storage(String),

    /// Конфиг не разбирается. Имя файла ЗДЕСЬ НУЖНО: без него человек не знает,
    /// какой из двух файлов (`voice.toml` или `meet.toml`) чинить.
    #[error("{file} не разбирается: {reason}")]
    Config { file: String, reason: String },

    /// Расшифровка отменена (пользователем или таймаутом реестра).
    #[error("расшифровка отменена")]
    Cancelled,

    /// Все слоты параллельных расшифровок заняты; внутри — сколько их идёт.
    /// Отказ мгновенный: молчаливое ожидание на семафоре означало бы открытую
    /// плашку без единого кадра и без объяснения.
    #[error("уже идёт {0} расшифровка(ок)")]
    Busy(usize),

    /// Вложение не голосовое и не аудио. Внутри — вид вложения, каким его
    /// назвал плагин: человеку надо видеть, ЧТО он пытался расшифровать.
    #[error("это не голосовое сообщение (вид вложения: {0})")]
    NotVoice(String),

    /// Запись длиннее потолка [`crate::VoiceConfig::max_duration_secs`].
    ///
    /// Потолок называется вслух: «слишком длинное» без числа не даёт человеку
    /// ни одного способа понять, насколько именно.
    #[error("запись длиной {secs} с длиннее потолка {limit} с")]
    TooLong { secs: i64, limit: u64 },

    /// Нужного внешнего бинаря нет.
    ///
    /// `what` — `ffmpeg` или `whisper`; `hint` — что с этим делать. Отдельный
    /// вариант, а не [`Self::Proc`] с `spawn_failed`: отсутствие ffmpeg
    /// чинится установкой, а `spawn_failed` от упавшего процесса — нет, и
    /// подсказки у них разные.
    #[error("{what} не найден: {hint}")]
    MissingBinary { what: &'static str, hint: String },

    /// Файла модели whisper нет.
    #[error("модель распознавания не найдена: {0}")]
    MissingModel(String),

    /// Облако выбрано движком, но ключ polza.ai не задан.
    ///
    /// Текст НЕ содержит ни ключа, ни его маски: сообщение уезжает и в ответ
    /// HTTP, и в журнал.
    #[error("ключ polza.ai не задан: облачная расшифровка недоступна")]
    NoCloudKey,
}

/// К какому классу отказа относится ошибка.
///
/// Ядро про HTTP не знает и знать не должно, но выбор кода ответа обязан быть
/// ОДИНАКОВЫМ во всех трёх плагинах — иначе общий фронт-модуль (Ф5) получил бы
/// от VK `409`, а от Bitrix `500` на одну и ту же причину. Поэтому решение
/// «какого рода этот отказ» принимается ЗДЕСЬ, один раз, а перевод класса в
/// конкретный конструктор `ApiError` остаётся за плагином: таблицы кодов у них
/// свои и заморожены контрактными тестами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalClass {
    /// Клиент попросил невозможное: не голосовое, слишком длинное. `400`.
    Input,
    /// Состояние машины: нет ключа, нет модели, нет бинаря, занято. `409`.
    State,
    /// Внешняя причина, которая может пройти сама: облако, процесс,
    /// отменённый прогон. `503`.
    Unavailable,
    /// Наша поломка: стор, конфиг. `500`.
    Internal,
}

impl VoiceError {
    /// Класс отказа. Один на три плагина — см. [`RefusalClass`].
    #[must_use]
    pub fn class(&self) -> RefusalClass {
        match self {
            Self::NotVoice(_) | Self::TooLong { .. } => RefusalClass::Input,
            Self::MissingBinary { .. }
            | Self::MissingModel(_)
            | Self::NoCloudKey
            | Self::Busy(_) => RefusalClass::State,
            Self::Cloud(_) | Self::Proc(_) | Self::Audio(_) | Self::Cancelled => {
                RefusalClass::Unavailable
            }
            Self::Storage(_) | Self::Config { .. } => RefusalClass::Internal,
        }
    }
}

impl VoiceError {
    /// Машиночитаемый код. Плагин различает случаи по нему, а не по тексту.
    ///
    /// Коды вложенных отказов ПРОБРАСЫВАЮТСЯ как есть: `metal_unavailable` и
    /// `cloud_no_funds` — это ровно те слова, по которым фронт показывает
    /// человеку разные подсказки, и заменить их на общее `voice_proc` значило
    /// бы потерять единственную полезную часть отказа.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Proc(e) => e.code,
            Self::Cloud(e) => e.code(),
            Self::Audio(_) => "voice_audio",
            Self::Storage(_) => "voice_storage",
            Self::Config { .. } => "voice_config",
            Self::Cancelled => "cancelled",
            Self::Busy(_) => "voice_busy",
            Self::NotVoice(_) => "voice_not_audio",
            Self::TooLong { .. } => "voice_too_long",
            // Код зависит от того, КАКОГО бинаря нет: `voice_no_ffmpeg` — это
            // «поставьте ffmpeg», а `voice_no_whisper` — «поставьте whisper.cpp
            // либо выберите облако». Один код на двоих отправил бы половину
            // пользователей чинить не то.
            Self::MissingBinary { what, .. } => {
                if *what == crate::tools::FFMPEG_NAME {
                    "voice_no_ffmpeg"
                } else {
                    "voice_no_whisper"
                }
            }
            Self::MissingModel(_) => "voice_no_model",
            Self::NoCloudKey => "voice_no_key",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Код вложенного отказа доживает до вызывающего, а не схлопывается в
    /// «внутреннюю ошибку».
    #[test]
    fn the_code_of_a_nested_failure_survives() {
        let e: VoiceError = ProcError::new("metal_unavailable", "GPU не подтверждён", "").into();
        assert_eq!(e.code(), "metal_unavailable");

        let e: VoiceError = CloudError::Status {
            status: 402,
            api_code: None,
            message: "нет средств".into(),
        }
        .into();
        assert_eq!(e.code(), "cloud_no_funds");

        assert_eq!(VoiceError::Busy(3).code(), "voice_busy");
        assert_eq!(VoiceError::Cancelled.code(), "cancelled");
    }

    /// Текст ошибки уходит и в лог, и пользователю — секретов в нём быть не
    /// может.
    #[test]
    fn display_is_never_empty_and_names_the_config_file() {
        let e = VoiceError::Config {
            file: "voice.toml".into(),
            reason: "ожидалась строка".into(),
        };
        let text = e.to_string();
        assert!(text.contains("voice.toml"), "{text}");
        assert!(text.contains("ожидалась строка"), "{text}");
        assert!(!VoiceError::Busy(1).to_string().is_empty());
    }
}
