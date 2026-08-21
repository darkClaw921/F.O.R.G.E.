//! Голосовые сообщения: связка плагина с ядром `forge-voice`.
//!
//! Близнец `plugins/vk/src/voice.rs` и `plugins/telegram/src/voice.rs` — и это
//! требование, а не совпадение: текст под голосовым рисует ОДИН фронт-модуль на
//! три мессенджера, поэтому коды отказов, форма кадров и правила выбора движка
//! обязаны совпадать. Всё общее живёт в ядре (`plugins/voice`), здесь — только
//! перевод понятий.
//!
//! # ЧЕМ BITRIX ОТЛИЧАЕТСЯ ОТ ОСТАЛЬНЫХ ДВУХ
//!
//! 1. **Диалог — не число.** `chat123`, `sg45`, `77` (пользователь) — поэтому
//!    каталог кэша называется [`crate::media_cache::DialogKey::dir_name`], а не
//!    `peer_id.to_string()`. В ключ ядра уезжает та же строка: она уже
//!    проверена разбором и в путь попадает только через него.
//! 2. **ДЛИТЕЛЬНОСТИ НЕТ НИ В ОДНОМ ОТВЕТЕ ПОРТАЛА.** У VK и Telegram она
//!    приходит в метаданных вложения, а `im.v2.File` её не отдаёт вовсе —
//!    поэтому [`forge_voice::VoiceRequest`] здесь всегда без длительности, и
//!    потолок проверяется ПОЗЖЕ, по фактически декодированному звуку
//!    ([`forge_voice::audio::prepare`]). Это не обход правила, а единственный
//!    способ его соблюсти: врать «длительность неизвестна, значит подходит» и
//!    гнать в облако получасовую запись нельзя.
//! 3. **Лимит портала рвёт TCP вместо ответа.** Докачка идёт штатным путём
//!    плагина ([`crate::routes::media::download_attachment`]), который эту
//!    особенность уже переживает: обрыв превращается в `503 not_connected`, а
//!    не в панику. Расшифровка на этом просто не начинается.

use std::sync::Arc;

use forge_voice::{RefusalClass, VoiceError, VoiceKey};

use crate::config::BitrixConfig;
use crate::media_cache::{CacheKey, MediaKind, MediaMeta};
use crate::routes::error::ApiError;
use crate::state::B24State;

/// Вид вложения, который лестница считает голосовым.
pub const KIND_VOICE: &str = "voice";

/// Координаты вложения в терминах ядра.
///
/// `peer` — имя каталога диалога (`chat123`, `sg45`, `77`), то есть ровно то,
/// что уже лежит на диске в медиа-кэше. Аватар голосовым быть не может, и для
/// него функция отдаёт `None`.
#[must_use]
pub fn voice_key(key: &CacheKey) -> Option<VoiceKey> {
    match key {
        CacheKey::File {
            dialog,
            message_id,
            index,
        } => Some(VoiceKey::new(
            dialog.dir_name(),
            message_id.to_string(),
            u8::try_from(*index).unwrap_or(u8::MAX),
        )),
        CacheKey::Avatar { .. } => None,
    }
}

/// Основа имени файла вложения в медиа-кэше: `<message_id>.<index>.file`.
///
/// Дублирует приватный `CacheKey::stem`, и это осознанно: делать его публичным
/// значило бы разрешить собирать пути мимо [`crate::media_cache::path_for`].
/// Ядро дописывает к основе `.bin` само.
#[must_use]
pub fn cache_stem(key: &CacheKey) -> Option<String> {
    match key {
        CacheKey::File {
            message_id, index, ..
        } => Some(format!("{message_id}.{index}.{}", MediaKind::File)),
        CacheKey::Avatar { .. } => None,
    }
}

/// Вид вложения по метаданным кэша.
///
/// Единственный признак — MIME, записанный при скачивании: портал отдаёт
/// голосовое обычным файлом (`audio/mpeg`, `audio/mp4`, `audio/ogg`), и
/// отдельного типа «голосовое» в `im.v2` нет вовсе.
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
/// Признак ровно один — MIME из меты кэша: у портала нет ни флага «голосовое»,
/// ни длительности, ни тем более огибающей (см. шапку модуля). Всё, что
/// начинается на `audio/`, рисуется карточкой с волной.
#[must_use]
pub fn is_audio(meta: &MediaMeta) -> bool {
    kind_of(meta) == KIND_VOICE
}

/// Уже посчитанная огибающая вложения. Пустой вектор — «ещё нет».
///
/// Чтение синхронное и дешёвое: один маленький JSON рядом с байтами, и только
/// для тех вложений, которые лента вообще считает аудио. Ошибок не отдаёт
/// никогда — отсутствие волны это рабочее состояние карточки.
#[must_use]
pub fn cached_waveform(cfg: &BitrixConfig, key: &CacheKey) -> Vec<i32> {
    let (bin, _) = crate::media_cache::path_for(cfg, key);
    forge_voice::waveform::read_cached(&bin).unwrap_or_default()
}

/// Подставляет посчитанные огибающие в вложения страницы ленты.
///
/// # Почему это отдельный проход, а не поле проекции
///
/// [`crate::attach::media_from_file`] — чистая функция от дескриптора портала,
/// и знать про диск ей нечем и незачем. Огибающая же живёт РЯДОМ С БАЙТАМИ, то
/// есть появляется только после скачивания вложения, причём вообще в другой
/// момент времени. Поэтому она добавляется отдельным проходом там, где
/// страница уже собрана.
///
/// # Сколько это стоит
///
/// Одно чтение маленького JSON НА АУДИО-ВЛОЖЕНИЕ, а не на сообщение: карточки
/// с волной в ленте единичны, у остальных видов сайдкара нет по построению и
/// файловая система не трогается вовсе. Страница из сотни текстовых сообщений
/// не делает ни одного лишнего обращения к диску.
///
/// Ничего не найдено — поле остаётся пустым, фронт рисует детерминированную
/// заглушку, и волна появится при следующей выдаче ленты.
pub fn attach_waveforms(cfg: &BitrixConfig, dialog: &str, messages: &mut [crate::dto::MessageDto]) {
    use crate::attach::media_kind;
    let Some(dialog) = crate::media_cache::DialogKey::parse(dialog) else {
        return;
    };
    for msg in messages.iter_mut() {
        if msg.id <= 0 {
            continue;
        }
        for media in msg.media.iter_mut() {
            if media.kind != media_kind::VOICE && media.kind != media_kind::AUDIO {
                continue;
            }
            if !media.waveform.is_empty() {
                continue;
            }
            let key = CacheKey::File {
                dialog,
                message_id: msg.id,
                index: media.index,
            };
            media.waveform = cached_waveform(cfg, &key);
        }
    }
}

/// Считает огибающую вложения и кладёт сайдкар рядом с байтами.
///
/// Идемпотентна: если сайдкар уже есть, ffmpeg не запускается
/// ([`forge_voice::waveform::ensure`]).
///
/// # ОТКАЗ НЕ ЯВЛЯЕТСЯ ОШИБКОЙ КЭША
///
/// Ffmpeg не установлен, файл не аудио, файл битый — всё это `None` и ОДНА
/// внятная строка в журнале. Скачанный файл при этом уже лежит в кэше и уже
/// отдан человеку: волна — украшение карточки, и ронять из-за неё выдачу
/// вложения нельзя.
pub async fn ensure_waveform(
    state: &Arc<B24State>,
    key: &CacheKey,
    meta: &MediaMeta,
) -> Option<Vec<i32>> {
    if !is_audio(meta) {
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
                target: "forge_bitrix",
                key = %key.id(),
                "forge-bitrix: огибающая не посчитана, ffmpeg недоступен: {e}"
            );
            return None;
        }
    };
    match forge_voice::waveform::ensure(&bin, &ffmpeg).await {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::info!(
                target: "forge_bitrix",
                key = %key.id(),
                "forge-bitrix: огибающая не посчитана: {e}"
            );
            None
        }
    }
}

/// Ставит расчёт огибающей в фон и немедленно возвращает управление.
///
/// Вызывается СРАЗУ ПОСЛЕ `commit` медиа-кэша. В фон, а не в строку, ровно
/// потому, что человек в этот момент ждёт байты: декод минутного голосового —
/// это доли секунды, но привязывать к ним ответ роута незачем, а зависший
/// ffmpeg (потолок — [`forge_voice::waveform::DEFAULT_TIMEOUT_SECS`]) держал бы
/// запрос целиком.
///
/// Волна доедет до ленты со следующей выдачей `MediaDto` — ровно как задумано:
/// пока её нет, карточка рисует детерминированную заглушку.
pub fn spawn_waveform(state: &Arc<B24State>, key: CacheKey, meta: MediaMeta) {
    if !is_audio(&meta) {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let _ = ensure_waveform(&state, &key, &meta).await;
    });
}

/// Отказ ядра → конверт ошибки плагина.
///
/// HTTP-код выбирается по [`RefusalClass`] из ядра — ровно как в VK и Telegram:
/// одна причина обязана давать один код во всех трёх панелях. `code` в теле —
/// код ЯДРА (`voice_no_key`, `metal_unavailable`), а не имя из замороженной
/// таблицы [`crate::routes::error`]: по нему фронт показывает разные подсказки,
/// и схлопывать его в `invalid_state` значило бы потерять единственную полезную
/// часть отказа. Форма конверта при этом прежняя — `{"error":{code,message}}`.
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
    use crate::media_cache::DialogKey;

    fn key(dialog: &str) -> CacheKey {
        CacheKey::File {
            dialog: DialogKey::parse(dialog).unwrap(),
            message_id: 7001,
            index: 1,
        }
    }

    /// Мета аудио-вложения (единственный признак «это звук» — MIME).
    fn audio_meta() -> MediaMeta {
        MediaMeta::now(Some("audio/mpeg".into()), 8, Some("voice.mp3".into()))
    }

    /// Состояние на изолированных каталогах плюс `voice.toml` рядом с
    /// конфигом плагина — ровно там, где его ищет `paths_next_to`.
    ///
    /// `ffmpeg` — мок, печатающий известный PCM и отмечающий каждый запуск в
    /// журнале: без него тест зависел бы от того, что стоит на машине.
    #[cfg(unix)]
    fn state_with_mock_ffmpeg(dir: &std::path::Path) -> (Arc<B24State>, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("ffmpeg-runs.log");
        let ffmpeg = dir.join("ffmpeg-mock");
        std::fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\necho run >> {}\n\
                 i=0; while [ $i -lt 200 ]; do printf '\\000\\120'; i=$((i+1)); done\n\
                 i=0; while [ $i -lt 200 ]; do printf '\\000\\010'; i=$((i+1)); done\n",
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

        let state = B24State::new(BitrixConfig {
            cache_dir: dir.join("cache"),
            config_path: dir.join("bitrix.toml"),
            prompts_path: dir.join("prompts.json"),
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            ..BitrixConfig::default()
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

    /// **ОГИБАЮЩАЯ ПОЯВЛЯЕТСЯ ПОСЛЕ ПОПАДАНИЯ ФАЙЛА В КЭШ — И РОВНО ОДИН РАЗ.**
    ///
    /// Портал огибающую не отдаёт вовсе, поэтому единственный момент, когда её
    /// можно посчитать, — это когда оригинал лёг на диск. Повторный вызов
    /// обязан читать сайдкар, а не запускать ffmpeg заново: скачивание одного
    /// вложения повторяется сколько угодно, форма записи — никогда.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_audio_file_gets_its_waveform_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let (state, log) = state_with_mock_ffmpeg(dir.path());
        let k = key("chat123");
        let meta = audio_meta();

        // Огибающей нет, пока нет файла.
        assert!(cached_waveform(&state.config, &k).is_empty());

        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();
        let first = ensure_waveform(&state, &k, &meta).await.unwrap();
        assert_eq!(first.len(), forge_voice::waveform::MAX_WAVEFORM.min(400));
        assert!(first
            .iter()
            .all(|v| (0..=forge_voice::waveform::WAVEFORM_MAX_VALUE).contains(v)));
        assert_eq!(runs(&log), 1);

        // Сайдкар лежит РЯДОМ с байтами и назван по ключу вложения.
        let (bin, _) = crate::media_cache::path_for(&state.config, &k);
        let side = forge_voice::waveform::sidecar_for(&bin);
        assert!(side.exists(), "{}", side.display());
        assert_eq!(side.file_name().unwrap(), "7001.1.file.wave.json");
        assert_eq!(cached_waveform(&state.config, &k), first);

        // Повторный commit того же ключа ffmpeg не запускает.
        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();
        assert_eq!(ensure_waveform(&state, &k, &meta).await.unwrap(), first);
        assert_eq!(runs(&log), 1, "огибающая пересчитана заново");

        // Соседний индекс — свой сайдкар и своя огибающая.
        let neighbour = CacheKey::File {
            dialog: DialogKey::parse("chat123").unwrap(),
            message_id: 7001,
            index: 0,
        };
        assert!(cached_waveform(&state.config, &neighbour).is_empty());
    }

    /// **НЕ-АУДИО И БИТЫЙ ФАЙЛ НЕ РОНЯЮТ НИЧЕГО.**
    ///
    /// Волна — украшение карточки; отсутствие её означает заглушку на фронте, а
    /// не ошибку кэша, и подавно не панику.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_audio_or_broken_file_is_simply_left_without_a_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let (state, log) = state_with_mock_ffmpeg(dir.path());
        let k = key("chat123");

        // PDF: ffmpeg даже не запускается — вид вложения известен из меты.
        let pdf = MediaMeta::now(Some("application/pdf".into()), 3, Some("a.pdf".into()));
        crate::media_cache::put_atomic(&state.config, &k, b"%PDF", &pdf)
            .await
            .unwrap();
        assert!(ensure_waveform(&state, &k, &pdf).await.is_none());
        assert_eq!(runs(&log), 0);
        assert!(cached_waveform(&state.config, &k).is_empty());

        // Аудио по мете, но файла на диске нет — внятный отказ, не паника.
        let ghost = CacheKey::File {
            dialog: DialogKey::parse("sg9").unwrap(),
            message_id: 42,
            index: 0,
        };
        assert!(ensure_waveform(&state, &ghost, &audio_meta()).await.is_none());
        assert!(cached_waveform(&state.config, &ghost).is_empty());
    }

    /// **БЕЗ FFMPEG ЛЕНТА И КЭШ РАБОТАЮТ КАК РАБОТАЛИ.**
    #[tokio::test]
    async fn a_missing_ffmpeg_costs_only_the_waveform() {
        let dir = tempfile::tempdir().unwrap();
        // `voice.toml` указывает на несуществующий бинарь, а PATH тут ни при
        // чём: явный путь проверяется первым.
        std::fs::write(
            dir.path().join("voice.toml"),
            format!(
                "ffmpeg_path = {:?}\n",
                dir.path().join("нет-такого-ffmpeg").display().to_string()
            ),
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("bitrix.toml"),
            prompts_path: dir.path().join("prompts.json"),
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            ..BitrixConfig::default()
        });

        let k = key("chat123");
        let meta = audio_meta();
        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();
        assert!(ensure_waveform(&state, &k, &meta).await.is_none());

        // Файл в кэше остался целым и отдаётся как прежде.
        let cached = crate::media_cache::get(&state.config, &k).await.unwrap();
        assert_eq!(cached.meta.mime.as_deref(), Some("audio/mpeg"));
        assert!(cached_waveform(&state.config, &k).is_empty());

        // Постановка в фон тоже молчит и ничего не роняет.
        spawn_waveform(&state, k, meta);
    }

    /// **ЕСТЬ КЭШ — ЕСТЬ ВОЛНА В DTO; НЕТ КЭША — ПУСТО И БЕЗ ОШИБКИ.**
    ///
    /// И ни одного чтения диска ради вложений, у которых волны не бывает.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_feed_shows_the_waveform_the_backend_computed() {
        use crate::attach::{media_kind, MediaDto};
        let dir = tempfile::tempdir().unwrap();
        let (state, _log) = state_with_mock_ffmpeg(dir.path());
        let k = key("chat123");
        let meta = audio_meta();
        crate::media_cache::put_atomic(&state.config, &k, b"OggS____", &meta)
            .await
            .unwrap();
        let computed = ensure_waveform(&state, &k, &meta).await.unwrap();

        let msg = |id: i64, media: Vec<MediaDto>| crate::dto::MessageDto {
            id,
            dialog_id: "chat123".into(),
            media,
            ..Default::default()
        };
        let voice = |index: u32| MediaDto {
            index,
            file_id: 95,
            kind: media_kind::VOICE.to_string(),
            ..MediaDto::default()
        };

        let mut page = vec![
            msg(7001, vec![voice(0), voice(1)]),
            // Кэша нет — поле остаётся пустым, фронт нарисует заглушку.
            msg(9999, vec![voice(1)]),
            // Не-аудио вложение и сообщение без вложений диск не трогают.
            msg(
                7003,
                vec![MediaDto {
                    index: 0,
                    kind: media_kind::FILE.to_string(),
                    ..MediaDto::default()
                }],
            ),
            msg(7005, Vec::new()),
        ];
        attach_waveforms(&state.config, "chat123", &mut page);

        assert_eq!(page[0].media[1].waveform, computed, "ключ сайдкара — ключ вложения");
        assert!(page[0].media[0].waveform.is_empty(), "соседний индекс — своя запись");
        assert!(page[1].media[0].waveform.is_empty());
        assert!(page[2].media[0].waveform.is_empty());

        // Неразбираемый диалог — просто пустые волны, а не паника и не поход в
        // файловую систему по строке из URL.
        let mut hostile = vec![msg(7001, vec![voice(1)])];
        attach_waveforms(&state.config, "../../etc", &mut hostile);
        assert!(hostile[0].media[0].waveform.is_empty());
    }

    /// Вид вложения решает, будет ли волна вообще.
    #[test]
    fn only_audio_gets_a_waveform() {
        let m = |mime: &str| MediaMeta {
            mime: Some(mime.to_string()),
            ..MediaMeta::default()
        };
        assert!(is_audio(&m("audio/ogg")));
        assert!(is_audio(&m(" audio/mpeg ")));
        assert!(!is_audio(&m("image/png")));
        assert!(!is_audio(&m("application/pdf")));
        assert!(!is_audio(&MediaMeta::default()));
    }

    /// **КЛЮЧ ЯДРА НЕСЁТ ИМЯ КАТАЛОГА ДИАЛОГА, А НЕ СЫРУЮ СТРОКУ ИЗ URL.**
    ///
    /// У портала три вида диалогов, и все три обязаны давать разные каталоги —
    /// иначе расшифровка беседы `chat123` перетёрла бы расшифровку пользователя
    /// `123`.
    #[test]
    fn the_core_key_uses_the_dialog_directory_name() {
        assert_eq!(voice_key(&key("chat123")).unwrap().peer, "chat123");
        assert_eq!(voice_key(&key("sg45")).unwrap().peer, "sg45");
        assert_eq!(voice_key(&key("77")).unwrap().peer, "77");
        assert_ne!(
            voice_key(&key("chat123")).unwrap().peer,
            voice_key(&key("123")).unwrap().peer
        );

        let k = voice_key(&key("chat123")).unwrap();
        assert_eq!(k.message_id, "7001");
        assert_eq!(k.index, 1);
        assert_eq!(cache_stem(&key("chat123")).unwrap(), "7001.1.file");
    }

    /// Аватар не вложение сообщения — расшифровывать его нечего и не по чему.
    #[test]
    fn an_avatar_is_not_transcribable() {
        let avatar = CacheKey::Avatar { user_id: 5 };
        assert!(voice_key(&avatar).is_none());
        assert!(cache_stem(&avatar).is_none());
    }

    /// Вид вложения читается из MIME; всё, что не аудио, называется вслух.
    #[test]
    fn the_kind_comes_from_the_mime_of_the_cached_file() {
        let m = |mime: &str| MediaMeta {
            mime: Some(mime.to_string()),
            ..MediaMeta::default()
        };
        for mime in ["audio/mpeg", "audio/mp4", "audio/ogg", " audio/wav "] {
            assert_eq!(kind_of(&m(mime)), KIND_VOICE, "{mime}");
        }
        assert_eq!(kind_of(&m("application/pdf")), "application/pdf");
        assert_eq!(kind_of(&MediaMeta::default()), "неизвестное вложение");
    }

    /// Классы отказов переводятся в те же коды, что в VK и Telegram: это
    /// прерквизит общего фронт-модуля.
    #[test]
    fn the_error_mapping_matches_the_other_plugins() {
        use axum::http::StatusCode;
        let cases = [
            (
                VoiceError::NotVoice("application/pdf".into()),
                StatusCode::BAD_REQUEST,
                "voice_not_audio",
            ),
            (
                VoiceError::TooLong { secs: 1000, limit: 900 },
                StatusCode::BAD_REQUEST,
                "voice_too_long",
            ),
            (VoiceError::NoCloudKey, StatusCode::CONFLICT, "voice_no_key"),
            (VoiceError::Busy(3), StatusCode::CONFLICT, "voice_busy"),
            (
                VoiceError::Audio("файла нет".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "voice_audio",
            ),
        ];
        for (err, status, code) in cases {
            let api = map_voice_error(&err);
            assert_eq!(api.status, status, "{err}");
            assert_eq!(api.body.code, code, "{err}");
        }
    }
}
