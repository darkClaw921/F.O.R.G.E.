//! Расшифровка голосовых сообщений по требованию.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/telegram/voice/:dialog_id/:message_id/:index/transcribe` | запустить расшифровку |
//! | GET | `/api/telegram/voice/:dialog_id/:message_id/:index/transcript` | отдать готовую расшифровку |
//!
//! # ПРЕФИКС `telegram`, А НЕ `tg`
//!
//! План фичи писал пути как `/api/tg/voice/...`, но у этого плагина ВСЕ роуты
//! живут под `/api/telegram/` — и таблица в шапке [`super`], и фронтовый
//! `js/telegram/api.js`, и контрактный тест, который их сверяет. Второй
//! префикс в одном плагине означал бы, что половина адресов пишется по одному
//! правилу, а половина по другому, и что каждый следующий роут — это ещё один
//! повод угадывать. Общий фронт-модуль (Ф5) получает базовый путь параметром,
//! поэтому ему разница безразлична.
//!
//! # `:index` ЕСТЬ, НО ОН ВСЕГДА НОЛЬ
//!
//! У сообщения Telegram ровно одно вложение, и ключ кэша индекса не содержит.
//! В адресе он всё равно присутствует: у VK вложений до десяти, а фронт-модуль
//! один на три панели, и адрес карточки в нём собирается одним и тем же кодом.
//! Любое значение, кроме нуля, — `400`: это ошибка клиента, а не «второе
//! вложение».
//!
//! Остальное — лестница, порядок отказов, дедуп и кадры WS — дословно как в
//! `plugins/vk/src/routes/voice.rs`; расхождения между панелями здесь были бы
//! багом, а не особенностью.

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use forge_voice::{TranscribeJob, VoiceRequest};

use crate::media_cache::{self, CacheKey, CachedMedia};
use crate::routes::dialogs::{require_authorized, require_client};
use crate::routes::error::ApiError;
use crate::routes::media::{download_media, fetch_cached};
use crate::state::TelegramState;
use crate::voice::{cache_stem, kind_of, map_voice_error, voice_key, KIND_VOICE, ONLY_INDEX};

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
pub fn router() -> Router<Arc<TelegramState>> {
    Router::new()
        .route(
            "/api/telegram/voice/:dialog_id/:message_id/:index/transcribe",
            post(post_transcribe),
        )
        .route(
            "/api/telegram/voice/:dialog_id/:message_id/:index/transcript",
            get(get_transcript),
        )
        .route(
            "/api/telegram/voice/settings",
            get(get_settings).put(put_settings),
        )
}

/// Разбор координат вложения. `kind` всегда `full`: расшифровывают оригинал.
fn parse_key(dialog: &str, mid: &str, index: &str) -> Result<CacheKey, ApiError> {
    let bad = || {
        ApiError::bad_input(
            "expected /api/telegram/voice/<dialog_id>/<message_id>/0/… with numeric ids; \
             a Telegram message carries exactly one attachment, so the index is always 0",
        )
    };
    // Индекс проверяется ЗДЕСЬ, а не молча отбрасывается: `…/1/transcribe`
    // означает, что клиент считает вложений несколько, и отвечать ему успехом
    // по нулевому вложению было бы враньём.
    if index.trim().parse::<u8>().ok() != Some(ONLY_INDEX) {
        return Err(bad());
    }
    CacheKey::parse(dialog, mid, Some("full")).ok_or_else(bad)
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

/// `POST /api/telegram/voice/:dialog_id/:message_id/:index/transcribe`.
///
/// # Errors
///
/// `400 invalid_input`, `409 not_authorized`, `503 not_connected`,
/// `404 no_media`, `400 voice_not_audio` / `400 voice_too_long`,
/// `409 voice_no_ffmpeg` / `voice_no_whisper` / `voice_no_model` /
/// `voice_no_key` / `voice_busy`, `500 voice_config`.
pub async fn post_transcribe(
    State(state): State<Arc<TelegramState>>,
    Path((dialog_id, message_id, index)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
) -> Result<Json<TranscribeResp>, ApiError> {
    let key = parse_key(&dialog_id, &message_id, &index)?;
    require_authorized(&state).await?;
    begin(&state, &key, force_requested(query.as_deref()))
        .await
        .map(Json)
}

/// Тело запуска расшифровки БЕЗ HTTP-обвязки.
///
/// Вынесено из [`post_transcribe`] ради авторазбора ([`crate::voice_auto`]):
/// он делает ровно то же самое, и второй экземпляр этой лестницы разошёлся бы
/// с первым на первой же правке. Авторазбор отличается двумя вещами: он не
/// спрашивает авторизацию (её уже спросил поток апдейтов) и никогда не просит
/// `force`.
///
/// # Errors
///
/// Те же, что у [`post_transcribe`], кроме `400 invalid_input` и
/// `409 not_authorized`.
pub(crate) async fn begin(
    state: &Arc<TelegramState>,
    key: &CacheKey,
    force: bool,
) -> Result<TranscribeResp, ApiError> {
    let key = *key;
    let vkey = voice_key(&key);
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
    // Скачивание в Telegram особенно дорого — файл тянется через MTProto
    // кусками по 512 КиБ, и минутное голосовое это несколько секунд.
    forge_voice::check(&cfg, &VoiceRequest::of_kind(KIND_VOICE)).map_err(|e| map_voice_error(&e))?;

    let hit = obtain_media(state, &key).await?;
    let request = VoiceRequest::of_kind(kind_of(&hit.meta))
        .with_duration_secs(hit.meta.duration_secs.map(i64::from));
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
        target: "forge_telegram",
        dialog_id = %key.dialog_id,
        message_id = key.message_id,
        run_id = %outcome.run_id(),
        engine = %engine,
        "forge-telegram: voice transcription started"
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
/// Докачка идёт ТЕМ ЖЕ путём, что `GET /api/telegram/media/...`, вместе с его
/// дедупом ([`fetch_cached`]): открытая карточка и кнопка «расшифровать» не
/// должны тянуть один файл дважды — по MTProto это буквально вдвое дольше.
async fn obtain_media(
    state: &Arc<TelegramState>,
    key: &CacheKey,
) -> Result<CachedMedia, ApiError> {
    if let Some(hit) = media_cache::get(&state.config, key).await {
        // Раскладка кэша описана ещё и в ядре, потому что она общая у трёх
        // плагинов. Сверка держит две половины вместе.
        debug_assert_eq!(
            forge_voice::audio::resolve_media(
                &state.config.cache_dir,
                &key.dialog_id.to_string(),
                &cache_stem(key),
            )
            .as_deref(),
            Some(hit.path.as_path()),
            "раскладка медиа-кэша Telegram разошлась с раскладкой forge-voice"
        );
        return Ok(hit);
    }
    let client = require_client(state).await?;
    fetch_cached(state, key, || {
        download_media(state.clone(), client.clone(), *key)
    })
    .await
}

/// `GET /api/telegram/voice/:dialog_id/:message_id/:index/transcript`.
///
/// Ни одного вызова Telegram и ни одного запуска движка: ответ читается из
/// локального файла стора, поэтому работает и при мёртвом соединении.
///
/// # Errors
///
/// `400 invalid_input`, `409 not_authorized`.
pub async fn get_transcript(
    State(state): State<Arc<TelegramState>>,
    Path((dialog_id, message_id, index)): Path<(String, String, String)>,
) -> Result<Json<TranscriptResp>, ApiError> {
    let key = parse_key(&dialog_id, &message_id, &index)?;
    require_authorized(&state).await?;

    Ok(Json(match state.voice_store().get(&voice_key(&key)) {
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

/// Ответ `GET`/`PUT` `/api/telegram/voice/settings`.
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

/// Тело `PUT /api/telegram/voice/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetVoiceSettingsReq {
    /// Движок: `local` либо `cloud`.
    pub engine: String,
}

/// `GET /api/telegram/voice/settings` — какой движок выбран и всё ли для него есть.
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
    State(state): State<Arc<TelegramState>>,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let cfg = state.voice_config().map_err(|e| map_voice_error(&e))?;
    Ok(Json(forge_voice::settings::describe(&cfg).into()))
}

/// `PUT /api/telegram/voice/settings` — переключить движок.
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
    State(state): State<Arc<TelegramState>>,
    Json(req): Json<SetVoiceSettingsReq>,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let engine = forge_voice::settings::parse_engine(&req.engine).ok_or_else(|| {
        ApiError::bad_input("engine: допустимы только \"local\" и \"cloud\"")
    })?;
    let (voice_path, meet_path) = state.voice_paths();
    let settings = forge_voice::settings::set_engine(&voice_path, &meet_path, engine)
        .map_err(|e| map_voice_error(&e))?;
    tracing::info!(
        target: "forge_telegram",
        engine = %engine.as_str(),
        "forge-telegram: voice engine switched"
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

    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};

    /// Пишет `voice.toml` рядом с конфигом плагина — там же, где его ищет
    /// [`TelegramState::voice_config`]. Ключ заведомо ненастоящий
    /// (`forge-v0m9k`), ffmpeg — заглушка: тест не имеет права зависеть от
    /// машины.
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

    fn config(dir: &std::path::Path) -> TelegramConfig {
        TelegramConfig {
            cache_dir: dir.join("cache"),
            session_path: dir.join("telegram.session"),
            prompts_path: dir.join("prompts.json"),
            ..TelegramConfig::default()
        }
    }

    /// Авторизованное состояние на изолированных каталогах.
    async fn authorized(dir: &std::path::Path) -> Arc<TelegramState> {
        let st = TelegramState::new(config(dir));
        st.set_login(LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: None,
                phone: None,
            },
        })
        .await;
        st
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send(
        state: Arc<TelegramState>,
        method: &str,
        uri: &str,
    ) -> axum::response::Response {
        crate::routes::build_router(state)
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

    /// **ФОРМА КООРДИНАТ — ПЕРВОЙ СТУПЕНЬЮ, И ИНДЕКС ОБЯЗАН БЫТЬ НУЛЁМ.**
    #[tokio::test]
    async fn the_shape_is_checked_first_and_the_index_must_be_zero() {
        let dir = tempfile::tempdir().unwrap();
        for uri in [
            // Второго вложения у сообщения Telegram не бывает.
            "/api/telegram/voice/-1001234/7001/1/transcript",
            "/api/telegram/voice/-1001234/7001/abc/transcript",
            "/api/telegram/voice/абв/7001/0/transcript",
            "/api/telegram/voice/-1001234/0/0/transcript",
        ] {
            let state = TelegramState::new(config(dir.path()));
            let resp = send(state, "GET", uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "invalid_input", "{uri}");
        }
    }

    /// **БЕЗ АККАУНТА — `409 not_authorized`, ОБА РОУТА.**
    #[tokio::test]
    async fn both_routes_demand_authorization() {
        let dir = tempfile::tempdir().unwrap();
        for (method, uri) in [
            ("GET", "/api/telegram/voice/-1001234/7001/0/transcript"),
            ("POST", "/api/telegram/voice/-1001234/7001/0/transcribe"),
        ] {
            let state = TelegramState::new(config(dir.path()));
            let resp = send(state, method, uri).await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{method} {uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
        }
    }

    /// **РАСШИФРОВКИ НЕТ — `200 {ready:false}`, А ГОТОВАЯ ЧИТАЕТСЯ ИЗ ФАЙЛА И
    /// ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК.**
    #[tokio::test]
    async fn a_stored_transcript_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let first = authorized(dir.path()).await;

        let json = body_json(
            send(first.clone(), "GET", "/api/telegram/voice/-1001234/7001/0/transcript").await,
        )
        .await;
        assert_eq!(json["ready"], false);
        assert_eq!(json["text"], "");

        let key = CacheKey::parse("-1001234", "7001", Some("full")).unwrap();
        first
            .voice_store()
            .put(
                &voice_key(&key),
                &Transcript::new("голосовое из телеграма", Engine::Local)
                    .with_language("ru")
                    .with_duration_ms(7000),
            )
            .unwrap();
        drop(first);

        // Состояние собирается заново на том же каталоге — ровно то, что
        // происходит при рестарте devforge.
        let second = authorized(dir.path()).await;
        let json = body_json(
            send(second, "GET", "/api/telegram/voice/-1001234/7001/0/transcript").await,
        )
        .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "голосовое из телеграма");
        assert_eq!(json["engine"], "local");
        assert_eq!(json["duration_ms"], 7000);
    }

    /// **ПОВТОРНЫЙ POST НА ГОТОВОЕ НЕ ЗАПУСКАЕТ ДВИЖОК.**
    #[tokio::test]
    async fn a_repeated_post_returns_the_ready_text_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let state = authorized(dir.path()).await;
        let key = CacheKey::parse("-1001234", "7001", Some("full")).unwrap();
        state
            .voice_store()
            .put(&voice_key(&key), &Transcript::new("уже готово", Engine::Cloud))
            .unwrap();

        let json = body_json(
            send(state.clone(), "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await,
        )
        .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "уже готово");
        assert!(json["run_id"].is_null());
        assert_eq!(state.voice.active(), 0);
    }

    /// **НЕНАСТРОЕННОЕ ОБЛАКО — `409 voice_no_key` ДО ЛЮБОГО СКАЧИВАНИЯ.**
    #[tokio::test]
    async fn an_unconfigured_engine_is_refused_before_any_download() {
        let dir = tempfile::tempdir().unwrap();
        // Конфига голосовых нет вовсе.
        let state = authorized(dir.path()).await;
        let resp = send(
            state.clone(),
            "POST",
            "/api/telegram/voice/-1001234/7001/0/transcribe",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_no_key");
        assert!(!json["error"]["message"].as_str().unwrap().contains("pza_"));
        assert_eq!(state.voice.active(), 0);
    }

    /// **БЕЗ СОЕДИНЕНИЯ ФАЙЛ НЕ СКАЧАТЬ — `503`, А НЕ `500`.**
    #[tokio::test]
    async fn a_missing_attachment_without_a_connection_is_503() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let state = authorized(dir.path()).await;
        let resp = send(state, "POST", "/api/telegram/voice/-1001234/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "not_connected");
    }
}
