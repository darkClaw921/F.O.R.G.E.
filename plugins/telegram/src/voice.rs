//! Голосовые сообщения: связка плагина с ядром `forge-voice`.
//!
//! Близнец `plugins/vk/src/voice.rs` — и это не совпадение, а требование: текст
//! под голосовым рисует ОДИН фронт-модуль на три мессенджера, поэтому и коды
//! отказов, и форма кадров, и правила выбора движка обязаны совпадать. Всё
//! общее живёт в ядре (`plugins/voice`), здесь — только перевод понятий.
//!
//! # ЧЕМ TELEGRAM ОТЛИЧАЕТСЯ ОТ VK
//!
//! Индексом вложения, точнее его отсутствием: у сообщения Telegram ровно одно
//! вложение, и [`crate::media_cache::CacheKey`] состоит из пары
//! `(dialog_id, message_id)`. Роут при этом ВСЁ РАВНО принимает `:index` и
//! требует нуля — ради общего фронт-модуля, у которого адрес карточки один на
//! три панели. Расхождение спрятано здесь, в [`voice_key`], а не в трёх местах
//! фронта.
//!
//! Второе отличие — формат: голосовые Telegram приходят `audio/ogg` (opus), и
//! whisper.cpp такой контейнер не открывает вовсе. Именно поэтому подготовка
//! аудио ([`forge_voice::audio`]) перекодирует вход всегда, а не «если надо».

use std::sync::Arc;

use forge_voice::{RefusalClass, VoiceError, VoiceKey};

use crate::config::TelegramConfig;
use crate::media_cache::{CacheKey, MediaKind, MediaMeta};
use crate::routes::error::ApiError;
use crate::state::TelegramState;

/// Вид вложения, который лестница считает голосовым.
pub const KIND_VOICE: &str = "voice";

/// Единственный допустимый индекс вложения.
///
/// У сообщения Telegram вложение одно. Индекс в адресе оставлен ради
/// одинаковости с VK и Bitrix, и любое значение, кроме нуля, — это ошибка
/// клиента, а не «второе вложение».
pub const ONLY_INDEX: u8 = 0;

/// Координаты вложения в терминах ядра.
#[must_use]
pub fn voice_key(key: &CacheKey) -> VoiceKey {
    VoiceKey::new(
        key.dialog_id.to_string(),
        key.message_id.to_string(),
        ONLY_INDEX,
    )
}

/// Основа имени файла вложения в медиа-кэше: `<message_id>.full`.
///
/// Индекса в ней НЕТ — в отличие от VK. Ядро дописывает `.bin` само.
#[must_use]
pub fn cache_stem(key: &CacheKey) -> String {
    format!("{}.{}", key.message_id, MediaKind::Full)
}

/// Вид вложения по метаданным кэша.
///
/// Признак — MIME, записанный при скачивании. Голосовое Telegram это
/// `audio/ogg`, кружок-видеосообщение — `video/mp4` (его расшифровывать
/// нечем: звуковой дорожки в кэше нет отдельно, а видео мы не качаем ради
/// звука), пересланный трек — `audio/mpeg`.
#[must_use]
pub fn kind_of(meta: &MediaMeta) -> String {
    match meta.mime.as_deref().map(str::trim) {
        Some(m) if m.starts_with("audio/") => KIND_VOICE.to_string(),
        Some(m) if !m.is_empty() => m.to_string(),
        _ => "неизвестное вложение".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Звуковая огибающая.
// ---------------------------------------------------------------------------

/// Считает ли плагин это вложение аудио, по которому имеет смысл строить волну.
///
/// Признак тот же, что у лестницы расшифровки, — MIME из меты кэша.
#[must_use]
pub fn is_audio(meta: &MediaMeta) -> bool {
    kind_of(meta) == KIND_VOICE
}

/// Уже посчитанная огибающая вложения. Пустой вектор — «ещё нет».
///
/// Читается только для ОРИГИНАЛА ([`MediaKind::Full`]): превью — картинка, и
/// огибающей у него не бывает по определению.
#[must_use]
pub fn cached_waveform(cfg: &TelegramConfig, key: &CacheKey) -> Vec<i32> {
    if key.kind != MediaKind::Full {
        return Vec::new();
    }
    let (bin, _) = crate::media_cache::path_for(cfg, key);
    forge_voice::waveform::read_cached(&bin).unwrap_or_default()
}

/// Подставляет посчитанные огибающие в вложения страницы ленты.
///
/// # Что здесь происходит и чего НЕ происходит
///
/// Атрибут `DocumentAttributeAudio.waveform` уже разобран проекцией
/// (`dto::media_dto`), и непустое поле НЕ ТРОГАЕТСЯ: пересчёт поверх точных
/// данных Telegram был бы чистой потерей. Досчитывается только пустое — то
/// есть обычные аудиофайлы, для которых бэкенд посчитал огибающую сам при
/// скачивании.
///
/// Одно чтение маленького JSON на АУДИО-вложение; у сообщений без звука
/// файловая система не трогается вовсе.
pub fn attach_waveforms(cfg: &TelegramConfig, messages: &mut [crate::dto::MessageDto]) {
    for msg in messages.iter_mut() {
        let Some(media) = msg.media.as_mut() else {
            continue;
        };
        if media.kind != KIND_VOICE && media.kind != "audio" {
            continue;
        }
        if !media.waveform.is_empty() || msg.id <= 0 {
            continue;
        }
        let Some(key) = CacheKey::parse(&msg.dialog_id, &msg.id.to_string(), Some("full")) else {
            continue;
        };
        media.waveform = cached_waveform(cfg, &key);
    }
}

/// Считает огибающую скачанного вложения и кладёт сайдкар рядом с байтами.
///
/// # АТРИБУТ TELEGRAM ВСЕГДА ПОБЕЖДАЕТ РАСЧЁТ
///
/// У голосовых огибающая приезжает вместе с сообщением
/// (`DocumentAttributeAudio.waveform`, см. `dto::audio_waveform`) — бесплатно и
/// точно. Считать её самим имеет смысл ровно для аудио БЕЗ атрибута: обычных
/// файлов и пересланных треков. Решение принимает вызывающий, передавая
/// `has_attribute`: здесь нет доступа к TL-объекту сообщения, а лезть за ним
/// второй раз в сеть ради флага было бы абсурдно.
///
/// Отказ (нет ffmpeg, файл не аудио) — `None` и одна строка в журнале: волна
/// это украшение карточки, и ронять из-за неё выдачу вложения нельзя.
pub async fn ensure_waveform(
    state: &Arc<TelegramState>,
    key: &CacheKey,
    meta: &MediaMeta,
    has_attribute: bool,
) -> Option<Vec<i32>> {
    if has_attribute || key.kind != MediaKind::Full || !is_audio(meta) {
        return None;
    }
    let (bin, _) = crate::media_cache::path_for(&state.config, key);
    if let Some(cached) = forge_voice::waveform::read_cached(&bin) {
        return Some(cached);
    }
    let ffmpeg = match state
        .voice_config()
        .map_err(|e| e.to_string())
        .and_then(|cfg| {
            forge_voice::tools::resolve_ffmpeg(cfg.ffmpeg_path.as_deref())
                .map_err(|e| e.to_string())
        }) {
        Ok(path) => path,
        Err(e) => {
            tracing::info!(
                target: "forge_telegram",
                key = %key.id(),
                "forge-telegram: огибающая не посчитана, ffmpeg недоступен: {e}"
            );
            return None;
        }
    };
    match forge_voice::waveform::ensure(&bin, &ffmpeg).await {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::info!(
                target: "forge_telegram",
                key = %key.id(),
                "forge-telegram: огибающая не посчитана: {e}"
            );
            None
        }
    }
}

/// Ставит расчёт огибающей в фон сразу после `commit` медиа-кэша.
///
/// В фон, а не в строку: человек в этот момент ждёт байты вложения.
pub fn spawn_waveform(
    state: &Arc<TelegramState>,
    key: CacheKey,
    meta: MediaMeta,
    has_attribute: bool,
) {
    if has_attribute || key.kind != MediaKind::Full || !is_audio(&meta) {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let _ = ensure_waveform(&state, &key, &meta, false).await;
    });
}

/// Отказ ядра → конверт ошибки плагина.
///
/// HTTP-код выбирается по [`RefusalClass`] из ядра — ровно как в VK и Bitrix:
/// одна причина обязана давать один код во всех трёх панелях. `code` в теле —
/// код ЯДРА (`voice_no_key`, `metal_unavailable`): по нему фронт показывает
/// разные подсказки, и схлопывать его в `invalid_state` значило бы потерять
/// единственную полезную часть отказа.
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
        CacheKey::parse("-1001234", "7001", Some("full")).unwrap()
    }

    /// Мета аудио-вложения (единственный признак «это звук» — MIME).
    fn audio_meta() -> MediaMeta {
        MediaMeta::now(Some("audio/ogg".into()), 8, Some("voice.ogg".into()))
    }

    /// Состояние на изолированных каталогах плюс `voice.toml` рядом с
    /// файлом сессии — ровно там, где его ищет `paths_next_to`.
    #[cfg(unix)]
    fn state_with_mock_ffmpeg(dir: &std::path::Path) -> (Arc<TelegramState>, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("ffmpeg-runs.log");
        let ffmpeg = dir.join("ffmpeg-mock");
        std::fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\necho run >> {}\n\
                 i=0; while [ $i -lt 200 ]; do printf '\\000\\120'; i=$((i+1)); done\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            dir.join("voice.toml"),
            format!("ffmpeg_path = {:?}\n", ffmpeg.display().to_string()),
        )
        .unwrap();
        std::fs::write(dir.join("meet.toml"), b"").unwrap();

        let state = TelegramState::new(TelegramConfig {
            cache_dir: dir.join("cache"),
            session_path: dir.join("telegram.session"),
            prompts_path: dir.join("prompts.json"),
            ..TelegramConfig::default()
        });
        (state, log)
    }

    /// Сколько раз запускался мок ffmpeg.
    #[cfg(unix)]
    fn runs(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// **АТРИБУТ TELEGRAM ПОБЕЖДАЕТ РАСЧЁТ, А БЕЗ АТРИБУТА СЧИТАЕМ САМИ.**
    ///
    /// Огибающая из `DocumentAttributeAudio.waveform` бесплатна и точна;
    /// запускать ради неё ffmpeg значило бы платить за то, что уже есть.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_attribute_wins_and_only_its_absence_starts_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let (state, log) = state_with_mock_ffmpeg(dir.path());
        let k = key();
        let meta = audio_meta();
        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();

        // Атрибут есть — расчёт не запускается вовсе.
        assert!(ensure_waveform(&state, &k, &meta, true).await.is_none());
        assert_eq!(runs(&log), 0);
        assert!(cached_waveform(&state.config, &k).is_empty());

        // Атрибута нет — огибающая посчитана и закэширована рядом с байтами.
        let w = ensure_waveform(&state, &k, &meta, false).await.unwrap();
        assert!(!w.is_empty());
        assert_eq!(runs(&log), 1);
        let (bin, _) = crate::media_cache::path_for(&state.config, &k);
        let side = forge_voice::waveform::sidecar_for(&bin);
        assert_eq!(side.file_name().unwrap(), "7001.full.wave.json");
        assert_eq!(cached_waveform(&state.config, &k), w);

        // Повторный вызов читает сайдкар, а не считает заново.
        assert_eq!(ensure_waveform(&state, &k, &meta, false).await.unwrap(), w);
        assert_eq!(runs(&log), 1, "огибающая пересчитана заново");
    }

    /// Превью и не-аудио огибающей не получают, и это не ошибка.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_thumbnail_or_a_document_never_gets_a_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let (state, log) = state_with_mock_ffmpeg(dir.path());

        let thumb = CacheKey::parse("-1001234", "7001", Some("thumb")).unwrap();
        crate::media_cache::put_atomic(&state.config, &thumb, b"jpeg", &audio_meta())
            .await
            .unwrap();
        assert!(ensure_waveform(&state, &thumb, &audio_meta(), false)
            .await
            .is_none());
        assert!(cached_waveform(&state.config, &thumb).is_empty());

        let pdf = MediaMeta::now(Some("application/pdf".into()), 4, None);
        let k = key();
        crate::media_cache::put_atomic(&state.config, &k, b"%PDF", &pdf)
            .await
            .unwrap();
        assert!(ensure_waveform(&state, &k, &pdf, false).await.is_none());
        assert_eq!(runs(&log), 0);

        // Постановка в фон на тех же входах тоже молчит.
        spawn_waveform(&state, thumb, audio_meta(), false);
        spawn_waveform(&state, k, pdf, false);
        spawn_waveform(&state, k, audio_meta(), true);
    }

    /// **ЕСТЬ КЭШ — ЕСТЬ ВОЛНА В DTO; АТРИБУТ ПЕРЕЗАПИСЫВАТЬ ЗАПРЕЩЕНО.**
    #[cfg(unix)]
    #[tokio::test]
    async fn the_feed_shows_the_waveform_without_overwriting_the_attribute() {
        use crate::dto::{MediaDto, MessageDto};
        let dir = tempfile::tempdir().unwrap();
        let (state, _log) = state_with_mock_ffmpeg(dir.path());
        let k = key();
        let meta = audio_meta();
        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();
        let computed = ensure_waveform(&state, &k, &meta, false).await.unwrap();

        let msg = |id: i32, media: Option<MediaDto>| MessageDto {
            id,
            dialog_id: "-1001234".into(),
            media,
            ..Default::default()
        };
        let audio = |waveform: Vec<i32>| {
            Some(MediaDto {
                kind: "audio".into(),
                waveform,
                ..MediaDto::default()
            })
        };

        let mut page = vec![
            msg(7001, audio(Vec::new())),
            // Атрибут Telegram уже в DTO — трогать его нечем и незачем.
            msg(7001, audio(vec![7, 7, 7])),
            // Кэша нет — пусто, и это рабочее состояние карточки.
            msg(9999, audio(Vec::new())),
            // Фото и сообщение без вложений диск не трогают.
            msg(
                7001,
                Some(MediaDto {
                    kind: "photo".into(),
                    ..MediaDto::default()
                }),
            ),
            msg(7002, None),
        ];
        attach_waveforms(&state.config, &mut page);

        assert_eq!(page[0].media.as_ref().unwrap().waveform, computed);
        assert_eq!(page[1].media.as_ref().unwrap().waveform, vec![7, 7, 7]);
        assert!(page[2].media.as_ref().unwrap().waveform.is_empty());
        assert!(page[3].media.as_ref().unwrap().waveform.is_empty());
        assert!(page[4].media.is_none());
    }

    /// Вид вложения решает, будет ли волна вообще.
    #[test]
    fn only_audio_gets_a_waveform() {
        let m = |mime: &str| MediaMeta {
            mime: Some(mime.to_string()),
            ..MediaMeta::default()
        };
        assert!(is_audio(&m("audio/ogg")));
        assert!(!is_audio(&m("video/mp4")), "кружок звуковой дорожки не даёт");
        assert!(!is_audio(&MediaMeta::default()));
    }

    /// Ключ ядра собирается из пары координат Telegram, а индекс всегда ноль.
    #[test]
    fn the_core_key_pins_the_index_to_zero() {
        let k = voice_key(&key());
        assert_eq!(k.peer, "-1001234");
        assert_eq!(k.message_id, "7001");
        assert_eq!(k.index, ONLY_INDEX);
        assert_eq!(cache_stem(&key()), "7001.full");
    }

    /// Вид вложения читается из MIME; всё, что не аудио, называется вслух.
    #[test]
    fn the_kind_comes_from_the_mime_of_the_cached_file() {
        let m = |mime: &str| MediaMeta::now(Some(mime.to_string()), 10, None);
        assert_eq!(kind_of(&m("audio/ogg")), KIND_VOICE);
        assert_eq!(kind_of(&m("audio/mpeg")), KIND_VOICE);
        assert_eq!(kind_of(&m("video/mp4")), "video/mp4");
        assert_eq!(kind_of(&MediaMeta::now(None, 1, None)), "неизвестное вложение");
    }

    /// Классы отказов переводятся в те же коды, что в VK: это прерквизит
    /// общего фронт-модуля.
    #[test]
    fn the_error_mapping_matches_the_other_plugins() {
        use axum::http::StatusCode;
        let cases = [
            (VoiceError::NotVoice("video/mp4".into()), StatusCode::BAD_REQUEST, "voice_not_audio"),
            (VoiceError::NoCloudKey, StatusCode::CONFLICT, "voice_no_key"),
            (VoiceError::Busy(1), StatusCode::CONFLICT, "voice_busy"),
            (VoiceError::Cancelled, StatusCode::SERVICE_UNAVAILABLE, "cancelled"),
            (VoiceError::Storage("x".into()), StatusCode::INTERNAL_SERVER_ERROR, "voice_storage"),
        ];
        for (err, status, code) in cases {
            let api = map_voice_error(&err);
            assert_eq!(api.status, status, "{err}");
            assert_eq!(api.body.code, code, "{err}");
        }
    }
}
