//! Расшифровка голосовых сообщений по требованию.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/bitrix/voice/:dialog/:mid/:index/transcribe` | запустить расшифровку |
//! | GET | `/api/bitrix/voice/:dialog/:mid/:index/transcript` | отдать готовую расшифровку |
//!
//! # ЛЕСТНИЦА
//!
//! ```text
//! авторизация (409) ─▶ разбор координат (404/400)
//!        ─▶ готовый транскрипт (200, без запуска и без чтения настроек)
//!        ─▶ конфиг (500) ─▶ движок, бинари, ключ (409)
//!        ─▶ файл: кэш или докачка (503/404)
//!        ─▶ вид вложения (400) ─▶ дедуп и потолок (409) ─▶ старт
//! ```
//!
//! Порядок первых двух ступеней ОТЛИЧАЕТСЯ от VK — и намеренно: у Bitrix
//! авторизация стоит раньше разбора идентификаторов во ВСЕХ роутах
//! (`routes/media.rs`), потому что иначе роут работал бы оракулом валидности
//! идентификаторов портала для неподключённого клиента. Идентификаторы здесь
//! осмысленные (`chat123` — номер беседы портала), в отличие от индекса
//! вложения у VK, который описывает лишь форму запроса.
//!
//! # ДЛИТЕЛЬНОСТЬ ПРОВЕРЯЕТСЯ ТОЛЬКО ПОСЛЕ СКАЧИВАНИЯ
//!
//! Портал не отдаёт длительность аудио НИ В ОДНОМ ответе `im.v2.File`. Поэтому
//! потолок `max_duration_secs` здесь срабатывает позже, чем в VK и Telegram, —
//! в [`forge_voice::audio::prepare`], по фактически декодированному звуку. Код
//! отказа при этом тот же (`voice_too_long`), и разницы для фронта нет.
//!
//! # ОБРЫВ ДОКАЧКИ — ЭТО `503`, А НЕ ПАНИКА
//!
//! Известная особенность платформы: при исчерпании лимита портал РВЁТ TCP
//! вместо ответа с ошибкой. Докачка идёт штатным путём плагина
//! ([`download_attachment`]), который это уже переживает, а расшифровка на
//! оборванной докачке просто не начинается — ни прогона, ни занятого слота, ни
//! кадра `started`.

use std::sync::Arc;

use axum::extract::{Path, RawQuery};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use forge_voice::{TranscribeJob, VoiceRequest};

use crate::media_cache::{self, CacheKey, CachedMedia, DialogKey};
use crate::registry::B24Registry;
use crate::routes::dialogs::{require_authorized, require_client, require_connected};
use crate::routes::error::ApiError;
use crate::routes::media::{download_attachment, fetch_cached};
use crate::routes::Portal;
use crate::state::B24State;
use crate::voice::{cache_stem, kind_of, map_voice_error, voice_key, KIND_VOICE};

/// Ответ `POST …/transcribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscribeResp {
    /// Идентификатор прогона; `null` — прогон не запускался (уже готово).
    pub run_id: Option<String>,
    /// `true` ⇒ подключились к УЖЕ идущей расшифровке этого вложения.
    pub attached: bool,
    /// `true` ⇒ текст уже был готов и лежит в ответе.
    pub ready: bool,
    /// Каким движком считаем (или посчитали).
    pub engine: String,
    /// Готовый текст — только при `ready: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Ответ `GET …/transcript`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptResp {
    /// Есть ли расшифровка.
    pub ready: bool,
    /// Текст; пустая строка при `ready: false`.
    pub text: String,
    /// Язык, как его назвал движок.
    pub language: String,
    /// `local` | `cloud`; пустая строка при `ready: false`.
    pub engine: String,
    /// Длительность записи, мс.
    pub duration_ms: i64,
    /// Когда посчитано (unix), либо `null`.
    pub created_at: Option<i64>,
}

/// Роуты расшифровки.
pub fn router() -> Router<Arc<B24Registry>> {
    Router::new()
        .route(
            "/api/bitrix/voice/:dialog/:mid/:index/transcribe",
            post(post_transcribe),
        )
        .route(
            "/api/bitrix/voice/:dialog/:mid/:index/transcript",
            get(get_transcript),
        )
        .route(
            "/api/bitrix/voice/settings",
            get(get_settings).put(put_settings),
        )
}

/// Разбор координат вложения — ТЕМИ ЖЕ функциями, что у роута медиа.
///
/// Второго набора правил в плагине нет: `chat123` обязан разбираться одинаково
/// и для отдачи байтов, и для расшифровки, иначе одно и то же вложение имело бы
/// два адреса.
fn parse_key(dialog: &str, mid: &str, index: &str) -> Result<CacheKey, ApiError> {
    let dialog = DialogKey::parse(dialog)
        .ok_or_else(|| ApiError::unknown_dialog("no such dialog in this portal".to_string()))?;
    let message_id = media_cache::parse_entity_id(mid)
        .ok_or_else(|| ApiError::unknown_message("no such message in this portal".to_string()))?;
    let index = media_cache::parse_attach_index(index).ok_or_else(|| {
        ApiError::bad_input(format!(
            "the attachment index must be a number within 0..={}",
            media_cache::MAX_ATTACH_INDEX
        ))
    })?;
    Ok(CacheKey::File {
        dialog,
        message_id,
        index,
    })
}

/// Есть ли `force=1` в сырой query-строке.
fn force_requested(raw: Option<&str>) -> bool {
    raw.map(|q| {
        q.split('&').any(|pair| {
            matches!(pair.split_once('='), Some(("force", v)) if v == "1" || v == "true")
        })
    })
    .unwrap_or(false)
}

/// `POST /api/bitrix/voice/:dialog/:mid/:index/transcribe`.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog` / `unknown_message`,
/// `400 invalid_input`, `404 no_media` (сообщение удалено — портал байты больше
/// не отдаёт), `503 not_connected`, `400 voice_not_audio` / `voice_too_long`,
/// `409 voice_no_ffmpeg` / `voice_no_whisper` / `voice_no_model` /
/// `voice_no_key` / `voice_busy`, `500 voice_config`.
pub async fn post_transcribe(
    Portal(state): Portal,
    Path((dialog, mid, index)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
) -> Result<Json<TranscribeResp>, ApiError> {
    // Авторизация ДО разбора — как во всех роутах плагина.
    require_authorized(&state).await?;
    let key = parse_key(&dialog, &mid, &index)?;
    begin(&state, &key, force_requested(query.as_deref()))
        .await
        .map(Json)
}

/// Тело запуска расшифровки БЕЗ HTTP-обвязки.
///
/// Вынесено из [`post_transcribe`] ради авторазбора ([`crate::voice_auto`]): он
/// делает ровно то же самое, и второй экземпляр этой лестницы разошёлся бы с
/// первым на первой же правке. Авторазбор отличается двумя вещами: он не
/// спрашивает авторизацию (её уже спросил поток событий) и никогда не просит
/// `force`.
///
/// # Errors
///
/// Те же, что у [`post_transcribe`], кроме `409 not_authorized`;
/// `400 invalid_input` остаётся: аватар расшифровать нельзя.
pub(crate) async fn begin(
    state: &Arc<B24State>,
    key: &CacheKey,
    force: bool,
) -> Result<TranscribeResp, ApiError> {
    let key = *key;
    let vkey = voice_key(&key).ok_or_else(|| {
        ApiError::bad_input("only message attachments can be transcribed".to_string())
    })?;

    let store = state.voice_store();

    // Готовый текст отдаётся ДО чтения настроек: уже посчитанная расшифровка
    // обязана открываться даже с испорченным `voice.toml`.
    if !force {
        if let Some(t) = store.get(&vkey) {
            return Ok(TranscribeResp {
                run_id: None,
                attached: false,
                ready: true,
                engine: t.engine.as_str().to_string(),
                text: Some(t.text),
            });
        }
    }

    let cfg = state.voice_config().map_err(|e| map_voice_error(&e))?;
    // Ранняя проверка движка: «нет ffmpeg» и «нет ключа» видно ДО скачивания.
    forge_voice::check(&cfg, &VoiceRequest::of_kind(KIND_VOICE)).map_err(|e| map_voice_error(&e))?;

    let hit = obtain_media(state, &key).await?;
    // Длительности у портала нет НИ В ОДНОМ ответе — потолок проверит подготовка
    // аудио по декодированному звуку (см. шапку модуля).
    let request = VoiceRequest::of_kind(kind_of(&hit.meta));
    let plan = forge_voice::check(&cfg, &request).map_err(|e| map_voice_error(&e))?;

    let job = TranscribeJob {
        cfg: cfg.clone(),
        plan: plan.clone(),
        source: hit.path.clone(),
        cache_dir: state.config.cache_dir.clone(),
        store,
        key: vkey.clone(),
    };
    let engine = plan.engine_name().to_string();
    let outcome = state
        .voice
        .start(vkey, &engine, move |ctx| {
            forge_voice::transcribe::run(job, ctx)
        })
        .map_err(|e| map_voice_error(&e))?;

    tracing::debug!(
        target: "forge_bitrix",
        key = %key.id(),
        run_id = %outcome.run_id(),
        engine = %engine,
        "forge-bitrix: voice transcription started"
    );
    Ok(TranscribeResp {
        run_id: Some(outcome.run_id().to_string()),
        attached: !outcome.is_new(),
        ready: false,
        engine,
        text: None,
    })
}

/// Файл вложения: из кэша либо штатной докачкой плагина.
///
/// Порядок повторяет `GET /api/bitrix/media/...` целиком, включая память об
/// удалённых сообщениях: портал байты удалённого сообщения не отдаёт никогда, и
/// ходить за ними — это два обращения к порталу ради того же самого `404`.
async fn obtain_media(state: &Arc<B24State>, key: &CacheKey) -> Result<CachedMedia, ApiError> {
    if let Some(hit) = media_cache::get(&state.config, key).await {
        // Раскладка кэша описана ещё и в ядре, потому что она общая у трёх
        // плагинов. Сверка держит две половины вместе: разъедься они,
        // авторазбор Ф4 искал бы файл не там, и молча.
        debug_assert_eq!(
            cache_stem(key).and_then(|stem| forge_voice::audio::resolve_media(
                &state.config.cache_dir,
                &dir_name(key),
                &stem,
            )),
            Some(hit.path.clone()),
            "раскладка медиа-кэша Bitrix разошлась с раскладкой forge-voice"
        );
        return Ok(hit);
    }

    if let CacheKey::File { message_id, .. } = key {
        if state.is_deleted(*message_id) {
            return Err(ApiError::no_media(format!(
                "message {message_id} was deleted, the portal no longer serves its files"
            )));
        }
    }

    require_connected(state).await?;
    let client = require_client(state).await?;
    fetch_cached(state, key, || {
        download_attachment(state.clone(), client.clone(), *key)
    })
    .await
}

/// Имя каталога диалога в медиа-кэше.
fn dir_name(key: &CacheKey) -> String {
    match key {
        CacheKey::File { dialog, .. } => dialog.dir_name(),
        CacheKey::Avatar { .. } => String::new(),
    }
}

/// `GET /api/bitrix/voice/:dialog/:mid/:index/transcript`.
///
/// Ни одного вызова портала и ни одного запуска движка: ответ читается из
/// локального файла стора, поэтому работает и при исчерпанном лимите портала.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog` / `unknown_message`,
/// `400 invalid_input`.
pub async fn get_transcript(
    Portal(state): Portal,
    Path((dialog, mid, index)): Path<(String, String, String)>,
) -> Result<Json<TranscriptResp>, ApiError> {
    require_authorized(&state).await?;
    let key = parse_key(&dialog, &mid, &index)?;
    let vkey = voice_key(&key).ok_or_else(|| {
        ApiError::bad_input("only message attachments can be transcribed".to_string())
    })?;

    Ok(Json(match state.voice_store().get(&vkey) {
        Some(t) => TranscriptResp {
            ready: true,
            text: t.text,
            language: t.language,
            engine: t.engine.as_str().to_string(),
            duration_ms: t.duration_ms,
            created_at: Some(t.created_at),
        },
        None => TranscriptResp::default(),
    }))
}

/// Предупреждение «выбор сохранён, но работать он сейчас не будет».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceWarningDto {
    /// Машинный код отказа ядра (`voice_no_key`, `voice_no_model`, …).
    pub code: String,
    /// Текст для человека.
    pub message: String,
}

/// Ответ `GET`/`PUT` `/api/bitrix/voice/settings`.
///
/// Ключа polza.ai здесь НЕТ и быть не может — только признак «задан»
/// ([`Self::cloud_key_set`]). Этого хватает, чтобы UI написал «введите ключ», и
/// не хватает ни на что другое.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSettingsResp {
    /// `local` | `cloud`.
    pub engine: String,
    /// Задан ли ключ облака.
    pub cloud_key_set: bool,
    /// Чего не хватает выбранному движку; поля нет — всё на месте.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<VoiceWarningDto>,
}

impl From<forge_voice::settings::VoiceSettings> for VoiceSettingsResp {
    fn from(s: forge_voice::settings::VoiceSettings) -> Self {
        Self {
            engine: s.engine.as_str().to_string(),
            cloud_key_set: s.cloud_key_set,
            warning: s.warning.map(|w| VoiceWarningDto {
                code: w.code.to_string(),
                message: w.message,
            }),
        }
    }
}

/// Тело `PUT /api/bitrix/voice/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetVoiceSettingsReq {
    /// Движок: `local` либо `cloud`.
    pub engine: String,
}

/// `GET /api/bitrix/voice/settings` — какой движок выбран и всё ли для него есть.
///
/// # АВТОРИЗАЦИЯ ПЛАГИНА ЗДЕСЬ НЕ ТРЕБУЕТСЯ
///
/// В отличие от расшифровки, это не содержание переписки, а настройка машины,
/// общая для всех мессенджеров. Более того, чинить её нужно ИМЕННО тогда, когда
/// человек ещё не вошёл: вкладка настроек открывается до логина, и `409` в
/// ответ на «покажи выбранный движок» означал бы, что настроить расшифровку
/// нельзя, пока не настроишь аккаунт. Bearer-авторизация хоста роут всё равно
/// покрывает — он зарегистрирован до `auth_middleware`.
///
/// # Errors
///
/// `500 voice_config` — `voice.toml` или `meet.toml` не разбираются.
pub async fn get_settings(
    Portal(state): Portal,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let cfg = state.voice_config().map_err(|e| map_voice_error(&e))?;
    Ok(Json(forge_voice::settings::describe(&cfg).into()))
}

/// `PUT /api/bitrix/voice/settings` — переключить движок.
///
/// Выбор ЗАПИСЫВАЕТСЯ, даже если движок сейчас нерабочий (нет весов, нет
/// ключа): в ответе тогда приходит `warning`. Отказ означал бы порядок
/// «сначала скачай модель вслепую, потом переключай», хотя человек делает
/// ровно наоборот — переключает и читает, чего не хватает.
///
/// # Errors
///
/// `400 invalid_input` — слово движка не `local` и не `cloud`;
/// `500 voice_config` — файл не разбирается либо не пишется.
pub async fn put_settings(
    Portal(state): Portal,
    Json(req): Json<SetVoiceSettingsReq>,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let engine = forge_voice::settings::parse_engine(&req.engine).ok_or_else(|| {
        ApiError::bad_input("engine: допустимы только \"local\" и \"cloud\"")
    })?;
    let (voice_path, meet_path) = state.voice_paths();
    let settings = forge_voice::settings::set_engine(&voice_path, &meet_path, engine)
        .map_err(|e| map_voice_error(&e))?;
    tracing::info!(
        target: "forge_bitrix",
        engine = %engine.as_str(),
        "forge-bitrix: voice engine switched"
    );
    Ok(Json(settings.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use forge_voice::{Engine, Transcript};
    use tower::ServiceExt;

    use crate::config::BitrixConfig;
    use crate::login::{B24UserDto, LoginFlow};

    /// Пишет `voice.toml` рядом с конфигом плагина — там же, где его ищет
    /// [`B24State::voice_config`]. Ключ заведомо ненастоящий (`forge-v0m9k`),
    /// ffmpeg — заглушка: тест не имеет права зависеть от машины.
    fn write_voice_config(dir: &std::path::Path) {
        let ffmpeg = dir.join("ffmpeg-stub");
        std::fs::write(&ffmpeg, b"#!/bin/sh\nexit 1\n").unwrap();
        std::fs::write(
            dir.join("voice.toml"),
            format!(
                "engine = \"cloud\"\n\
                 polza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n\
                 ffmpeg_path = {:?}\n",
                ffmpeg.display().to_string()
            ),
        )
        .unwrap();
        std::fs::write(dir.join("meet.toml"), b"").unwrap();
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send(state: Arc<B24State>, method: &str, uri: &str) -> axum::response::Response {
        crate::routes::build_router(B24Registry::single(state))
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn config(dir: &std::path::Path) -> BitrixConfig {
        BitrixConfig {
            cache_dir: dir.join("cache"),
            config_path: dir.join("bitrix.toml"),
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            // Заведомо закрытый порт: ни один тест этого модуля не имеет права
            // уйти в живой портал, а состояние `authorized` без этого поля
            // отправило бы его именно туда.
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            ..BitrixConfig::default()
        }
    }

    /// Подключённый портал на изолированных каталогах.
    async fn authorized(dir: &std::path::Path) -> Arc<B24State> {
        let st = B24State::new(config(dir));
        st.set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "portal.bitrix24.ru".into(),
        })
        .await;
        st
    }

    /// **БЕЗ ПОРТАЛА — `409 not_authorized`, И ЭТО ПЕРВАЯ СТУПЕНЬ.**
    ///
    /// Даже с заведомо мусорными идентификаторами: иначе роут работал бы
    /// оракулом валидности идентификаторов портала.
    #[tokio::test]
    async fn authorization_comes_before_any_parsing() {
        let dir = tempfile::tempdir().unwrap();
        for (method, uri) in [
            ("GET", "/api/bitrix/voice/chat123/7001/0/transcript"),
            ("POST", "/api/bitrix/voice/chat123/7001/0/transcribe"),
            ("GET", "/api/bitrix/voice/..%2F..%2Fetc/7001/0/transcript"),
            ("POST", "/api/bitrix/voice/chat123/abc/99/transcribe"),
        ] {
            let state = B24State::new(config(dir.path()));
            let resp = send(state, method, uri).await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{method} {uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
        }
    }

    /// **РАСШИФРОВКИ НЕТ — `200 {ready:false}`; ГОТОВАЯ ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК.**
    #[tokio::test]
    async fn a_stored_transcript_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let first = authorized(dir.path()).await;

        let json = body_json(
            send(first.clone(), "GET", "/api/bitrix/voice/chat123/7001/1/transcript").await,
        )
        .await;
        assert_eq!(json["ready"], false);
        assert_eq!(json["text"], "");

        let key = parse_key("chat123", "7001", "1").unwrap();
        first
            .voice_store()
            .put(
                &voice_key(&key).unwrap(),
                &Transcript::new("голосовое из битрикса", Engine::Cloud)
                    .with_language("ru")
                    .with_duration_ms(9000),
            )
            .unwrap();
        drop(first);

        let second = authorized(dir.path()).await;
        let json =
            body_json(send(second, "GET", "/api/bitrix/voice/chat123/7001/1/transcript").await)
                .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "голосовое из битрикса");
        assert_eq!(json["duration_ms"], 9000);
    }

    /// **ПОВТОРНЫЙ POST НА ГОТОВОЕ НЕ ЗАПУСКАЕТ ДВИЖОК.**
    #[tokio::test]
    async fn a_repeated_post_returns_the_ready_text_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let state = authorized(dir.path()).await;
        let key = parse_key("chat123", "7001", "0").unwrap();
        state
            .voice_store()
            .put(
                &voice_key(&key).unwrap(),
                &Transcript::new("уже готово", Engine::Local),
            )
            .unwrap();

        let json = body_json(
            send(state.clone(), "POST", "/api/bitrix/voice/chat123/7001/0/transcribe").await,
        )
        .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "уже готово");
        assert!(json["run_id"].is_null());
        assert_eq!(state.voice.active(), 0);
    }

    /// **НЕНАСТРОЕННОЕ ОБЛАКО — `409 voice_no_key` ДО ЛЮБОГО ОБРАЩЕНИЯ К
    /// ПОРТАЛУ.**
    #[tokio::test]
    async fn an_unconfigured_engine_is_refused_before_any_download() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let resp = send(
            state.clone(),
            "POST",
            "/api/bitrix/voice/chat123/7001/0/transcribe",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_no_key");
        assert!(!json["error"]["message"].as_str().unwrap().contains("pza_"));
        assert_eq!(state.voice.active(), 0);
    }

    /// **ОБРЫВ ДОКАЧКИ НЕ РОНЯЕТ ЗАПРОС: `503`, А НЕ ПАНИКА И НЕ `500`.**
    ///
    /// Именно так выглядит исчерпанный лимит портала — он рвёт TCP вместо
    /// ответа с ошибкой. Здесь связь заведомо мертва, что даёт ту же ветку.
    #[tokio::test]
    async fn a_broken_download_answers_503_without_starting_a_run() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let state = authorized(dir.path()).await;
        state.note_down(crate::dto::now_unix(), "connection reset").await;

        let resp = send(
            state.clone(),
            "POST",
            "/api/bitrix/voice/chat123/7001/0/transcribe",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "not_connected");
        assert_eq!(state.voice.active(), 0, "прогон не запускался");
    }

    /// Мусорные координаты у АВТОРИЗОВАННОГО клиента — свои коды на каждую
    /// часть адреса, а не общий `400`.
    #[tokio::test]
    async fn every_part_of_the_address_has_its_own_refusal() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        for (uri, code) in [
            ("/api/bitrix/voice/..%2F..%2Fetc/7001/0/transcript", "unknown_dialog"),
            ("/api/bitrix/voice/chat123/abc/0/transcript", "unknown_message"),
            ("/api/bitrix/voice/chat123/7001/99/transcript", "invalid_input"),
        ] {
            let state = authorized(dir.path()).await;
            let resp = send(state, "GET", uri).await;
            assert_eq!(body_json(resp).await["error"]["code"], code, "{uri}");
        }
    }
}
