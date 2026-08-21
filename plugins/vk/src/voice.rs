//! Голосовые сообщения: связка плагина с ядром `forge-voice`.
//!
//! Ядро (`plugins/voice`) не знает ни про ВКонтакте, ни про axum, ни про
//! `VkState` — ровно как `forge-chat-analysis` (см. [`crate::analysis`]).
//! Здесь живёт всё, что переводит одно в другое:
//!
//! - координаты вложения VK ([`crate::media_cache::CacheKey`]) →
//!   [`forge_voice::VoiceKey`] и обратно;
//! - вид вложения из метаданных кэша → вид, понятный лестнице проверок;
//! - отказ ядра ([`forge_voice::VoiceError`]) → конверт ошибки плагина
//!   ([`ApiError`]).
//!
//! # ПОЧЕМУ КОНФИГ ЧИТАЕТСЯ НА КАЖДЫЙ ЗАПРОС
//!
//! `voice.toml` правят руками, и правят ровно тогда, когда что-то не работает:
//! вписывают ключ polza.ai, переключают движок. Прочитанный один раз при старте
//! конфиг означал бы «перезапустите devforge, чтобы настройка подействовала» —
//! то есть ровно тот совет, за который справедливо ругают. Файл маленький, его
//! чтение стоит одного `open` на нажатие кнопки.
//!
//! Из конструктора состояния конфиг всё-таки читается один раз — но только
//! ради потолков реестра ([`forge_voice::VoiceHub`]), которые нельзя менять на
//! лету, не бросив идущие прогоны.

use forge_voice::{RefusalClass, VoiceError, VoiceKey};

use crate::media_cache::{CacheKey, MediaKind, MediaMeta};
use crate::routes::error::ApiError;

/// Вид вложения, который лестница считает голосовым.
pub const KIND_VOICE: &str = "voice";

/// Координаты вложения в терминах ядра.
///
/// `peer_id` строкой — той же, что уезжает в DTO и WS-кадры: у ядра ключ
/// текстовый, потому что у трёх мессенджеров идентификаторы разного типа.
#[must_use]
pub fn voice_key(key: &CacheKey) -> VoiceKey {
    VoiceKey::new(key.peer_id.to_string(), key.message_id.to_string(), key.index)
}

/// Основа имени файла вложения в медиа-кэше: `<mid>.<idx>.full`.
///
/// Дублирует приватный `CacheKey::stem`, и это осознанно: делать его публичным
/// значило бы разрешить собирать пути мимо [`crate::media_cache::path_for`].
/// Здесь же нужна ровно основа — ядро дописывает к ней `.bin` само.
#[must_use]
pub fn cache_stem(key: &CacheKey) -> String {
    format!("{}.{}.{}", key.message_id, key.index, MediaKind::Full)
}

/// Вид вложения по метаданным кэша.
///
/// Единственный доступный признак — MIME, записанный при скачивании: сырого
/// вложения VK к этому моменту уже нет, а второй `messages.getById` ради одного
/// слова стоил бы трети секундного бюджета запросов (лимит 3 rps).
///
/// Голосовое VK приходит `audio/ogg`, пересланная запись — `audio/mpeg`, и оба
/// расшифровывать можно. Всё, что не `audio/*`, отдаётся лестнице как есть — и
/// она отвечает «это не голосовое», называя вид.
#[must_use]
pub fn kind_of(meta: &MediaMeta) -> String {
    match meta.mime.as_deref().map(str::trim) {
        Some(m) if m.starts_with("audio/") => KIND_VOICE.to_string(),
        Some(m) if !m.is_empty() => m.to_string(),
        _ => "неизвестное вложение".to_string(),
    }
}

/// Отказ ядра → конверт ошибки плагина.
///
/// HTTP-код выбирается по [`RefusalClass`], которая живёт в ядре: три плагина
/// обязаны отвечать на одну причину одним кодом, иначе общий фронт-модуль (Ф5)
/// получал бы от VK `409`, а от Bitrix `500`.
///
/// А вот `code` в теле — это код ЯДРА (`voice_no_key`, `metal_unavailable`), а
/// не одно из имён таблицы [`crate::routes::error`]. Так и задумано: коды
/// расшифровки различает фронт, и схлопывать их в `invalid_state` значило бы
/// потерять единственную полезную часть отказа. Форма конверта при этом
/// прежняя — `{"error":{code,message}}`.
#[must_use]
pub fn map_voice_error(e: &VoiceError) -> ApiError {
    use axum::http::StatusCode;
    let status = match e.class() {
        RefusalClass::Input => StatusCode::BAD_REQUEST,
        RefusalClass::State => StatusCode::CONFLICT,
        RefusalClass::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        RefusalClass::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError::new(status, e.code(), e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CacheKey {
        CacheKey::parse("2000000042", "7001", "1", Some("full")).unwrap()
    }

    /// Ключ ядра собирается из проверенного ключа кэша — и обратно читается
    /// без потерь.
    #[test]
    fn the_core_key_keeps_all_three_coordinates() {
        let k = voice_key(&key());
        assert_eq!(k.peer, "2000000042");
        assert_eq!(k.message_id, "7001");
        assert_eq!(k.index, 1);
        assert_eq!(cache_stem(&key()), "7001.1.full");
    }

    /// **ВИД ВЛОЖЕНИЯ: `audio/*` — ГОЛОСОВОЕ, ОСТАЛЬНОЕ НАЗЫВАЕТСЯ ВСЛУХ.**
    #[test]
    fn the_kind_comes_from_the_mime_of_the_cached_file() {
        let audio = |m: &str| MediaMeta::now(Some(m.to_string()), 10, None);
        for mime in ["audio/ogg", "audio/mpeg", "audio/mp4", " audio/ogg "] {
            assert_eq!(kind_of(&audio(mime)), KIND_VOICE, "{mime}");
        }
        assert_eq!(kind_of(&audio("image/jpeg")), "image/jpeg");
        assert_eq!(kind_of(&MediaMeta::now(None, 10, None)), "неизвестное вложение");

        // И лестница на таком виде отказывает своим кодом.
        let err = forge_voice::check(
            &forge_voice::VoiceConfig::default(),
            &forge_voice::VoiceRequest::of_kind(kind_of(&audio("image/jpeg"))),
        )
        .unwrap_err();
        assert_eq!(err.code(), "voice_not_audio");
    }

    /// **КАЖДЫЙ КЛАСС ОТКАЗА ПОЛУЧАЕТ СВОЙ HTTP-КОД, А `code` ОСТАЁТСЯ КОДОМ
    /// ЯДРА.**
    #[test]
    fn the_error_mapping_keeps_the_core_code() {
        use axum::http::StatusCode;
        let cases = [
            (VoiceError::NotVoice("photo".into()), StatusCode::BAD_REQUEST, "voice_not_audio"),
            (
                VoiceError::TooLong { secs: 999, limit: 900 },
                StatusCode::BAD_REQUEST,
                "voice_too_long",
            ),
            (VoiceError::NoCloudKey, StatusCode::CONFLICT, "voice_no_key"),
            (VoiceError::Busy(2), StatusCode::CONFLICT, "voice_busy"),
            (VoiceError::MissingModel("x".into()), StatusCode::CONFLICT, "voice_no_model"),
            (
                VoiceError::Audio("нет файла".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "voice_audio",
            ),
            (
                VoiceError::Storage("диск".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "voice_storage",
            ),
        ];
        for (err, status, code) in cases {
            let api = map_voice_error(&err);
            assert_eq!(api.status, status, "{err}");
            assert_eq!(api.body.code, code, "{err}");
            assert!(!api.body.message.is_empty());
        }
    }

    /// Ключ polza.ai не появляется в конверте ошибки ни при каком отказе.
    #[test]
    fn the_envelope_never_carries_the_key() {
        let err = VoiceError::Cloud(forge_voice::cloud::CloudError::Status {
            status: 401,
            api_code: None,
            message: "unauthorized".into(),
        });
        let api = map_voice_error(&err);
        assert!(!api.body.message.contains("pza_"), "{}", api.body.message);
    }
}
