//! Расшифровка голосовых сообщений по требованию.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/vk/voice/:id/:mid/:index/transcribe` | запустить расшифровку |
//! | GET | `/api/vk/voice/:id/:mid/:index/transcript` | отдать готовую расшифровку |
//!
//! # ЛЕСТНИЦА
//!
//! ```text
//! форма координат (400) ─▶ авторизация (409)
//!        ─▶ готовый транскрипт (200, без запуска и без чтения настроек)
//!        ─▶ конфиг (500) ─▶ движок, бинари, ключ (409)
//!        ─▶ файл: кэш или докачка (503/404)
//!        ─▶ вид вложения и длительность (400)
//!        ─▶ свободный слот и дедуп (409) ─▶ старт
//! ```
//!
//! Форма координат проверяется ПЕРВОЙ — как у `GET /api/vk/media/...` и по той
//! же причине: мусорный индекс это ошибка клиента в любом состоянии, и по `400`
//! про чужие беседы узнать нельзя. Дальше порядок обычный для плагина.
//!
//! # ПОЧЕМУ ДВИЖОК ПРОВЕРЯЕТСЯ ДО ФАЙЛА, А ВИД ВЛОЖЕНИЯ — ПОСЛЕ
//!
//! Вид вложения и его длительность известны только из метаданных кэша, то есть
//! ПОСЛЕ скачивания: сырого вложения у нас нет, а второй `messages.getById`
//! ради одного слова стоит трети секундного бюджета (лимит 3 rps). Зато
//! «ffmpeg не установлен» и «ключ polza.ai не введён» видно сразу — и отказать
//! этим ДО скачивания десяти мегабайт честнее, чем после.
//!
//! Поэтому лестница ядра ([`forge_voice::check`]) вызывается дважды: первый раз
//! с заведомо голосовым видом — ради проверки движка, второй раз с настоящим
//! видом и настоящей длительностью. Второй вызов — источник истины, первый —
//! экономия трафика.
//!
//! # ОТВЕТ УХОДИТ СРАЗУ, ТЕКСТ — КАДРАМИ WS
//!
//! `POST` возвращает `{run_id, attached, ready}` в момент, когда прогон только
//! запущен: расшифровка минутного голосового занимает секунды, а
//! пятнадцатиминутного — минуты, и держать HTTP-соединение всё это время
//! нельзя (прокси между браузером и devforge оборвёт его раньше). Прогресс
//! идёт кадрами `voice_transcribe_*` (см. [`crate::ws::protocol`]), финал —
//! `voice_transcribe_done`, а текст к этому моменту уже лежит в сторе, и его
//! можно взять `GET`-ом.
//!
//! # ГОТОВЫЙ ТРАНСКРИПТ НЕ ПЕРЕСЧИТЫВАЕТСЯ
//!
//! Повторный `POST` на уже расшифрованное вложение отвечает `200 {ready:
//! true}` и НЕ запускает движок: текст один и тот же, а прогон стоит секунд
//! процессорного времени либо денег в облаке. Пересчёт запрашивается явно —
//! `?force=1` (человек нажал «расшифровать заново», получив мусор).

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use forge_voice::{TranscribeJob, VoiceRequest};

use crate::media_cache::{self, CacheKey, CachedMedia};
use crate::routes::dialogs::{require_authorized, require_client, require_connected};
use crate::routes::error::ApiError;
use crate::routes::media::{download_attachment, fetch_cached};
use crate::state::VkState;
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
///
/// Расшифровки нет — это `200` с `ready: false`, а не `404`: «ещё не
/// расшифровывали» — нормальное состояние вложения, ровно как «ещё не
/// разбирали беседу» у анализа чата.
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
pub fn router() -> Router<Arc<VkState>> {
    // Имена сегментов — те же, что у `/api/vk/media/:id/:mid/:index`: axum
    // паникует при сборке, если один и тот же сегмент назван по-разному в
    // соседних роутах одного префикса.
    Router::new()
        .route("/api/vk/voice/:id/:mid/:index/transcribe", post(post_transcribe))
        .route("/api/vk/voice/:id/:mid/:index/transcript", get(get_transcript))
        .route(
            "/api/vk/voice/settings",
            get(get_settings).put(put_settings),
        )
}

/// Разбор координат вложения. Всегда `kind=full`: расшифровывают оригинал, а
/// не превью, и давать клиенту выбор здесь незачем.
fn parse_key(peer: &str, mid: &str, index: &str) -> Result<CacheKey, ApiError> {
    CacheKey::parse(peer, mid, index, Some("full")).ok_or_else(|| {
        ApiError::bad_input(
            "expected /api/vk/voice/<peer_id>/<message_id>/<index>/… with numeric ids \
             and index 0..9",
        )
    })
}

/// Есть ли `force=1` в сырой query-строке.
///
/// Разбор ручной, как во всех роутах плагина: экстрактор `Query<T>` отработал
/// бы ДО тела хендлера и ответил бы `400` раньше проверки авторизации, сломав
/// лестницу.
fn force_requested(raw: Option<&str>) -> bool {
    raw.map(|q| {
        q.split('&').any(|pair| {
            matches!(pair.split_once('='), Some(("force", v)) if v == "1" || v == "true")
        })
    })
    .unwrap_or(false)
}

/// `POST /api/vk/voice/:id/:mid/:index/transcribe`.
///
/// # Errors
///
/// `400 invalid_input` — координаты не разбираются; `409 not_authorized`;
/// `503 not_connected` / `404 no_media` — вложение не скачать; `400
/// voice_not_audio` / `400 voice_too_long`; `409 voice_no_ffmpeg` /
/// `409 voice_no_whisper` / `409 voice_no_model` / `409 voice_no_key` /
/// `409 voice_busy`; `500 voice_config` — `voice.toml` не разбирается.
pub async fn post_transcribe(
    State(state): State<Arc<VkState>>,
    Path((peer, mid, index)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
) -> Result<Json<TranscribeResp>, ApiError> {
    let key = parse_key(&peer, &mid, &index)?;
    require_authorized(&state).await?;
    begin(&state, &key, force_requested(query.as_deref()))
        .await
        .map(Json)
}

/// Тело запуска расшифровки БЕЗ HTTP-обвязки.
///
/// Вынесено из [`post_transcribe`] ради авторазбора ([`crate::voice_auto`]):
/// он делает ровно то же самое — по тому же ключу, тем же реестром, теми же
/// кадрами, — и второй экземпляр этой лестницы разошёлся бы с первым на первой
/// же правке. Отличается авторазбор ровно двумя вещами: он не спрашивает
/// авторизацию (её уже спросил лонгпол, принесший сообщение) и никогда не
/// просит `force`.
///
/// # Errors
///
/// Те же, что у [`post_transcribe`], кроме `400 invalid_input` (ключ уже
/// разобран) и `409 not_authorized`.
pub(crate) async fn begin(
    state: &Arc<VkState>,
    key: &CacheKey,
    force: bool,
) -> Result<TranscribeResp, ApiError> {
    let key = *key;
    let vkey = voice_key(&key);
    let store = state.voice_store();

    // Готовый текст отдаётся ДО чтения настроек, и это не мелочь: уже
    // посчитанная расшифровка обязана открываться даже с испорченным
    // `voice.toml` — ровно как сохранённый разбор беседы открывается при
    // мёртвой сети. Второй прогон дал бы тот же текст за те же деньги.
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

    // Ранняя проверка движка: отказать «нет ffmpeg» ДО скачивания вложения.
    // Вид вложения здесь заведомо голосовой — настоящий проверяется ниже.
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
        target: "forge_vk",
        key = %key.id(),
        run_id = %outcome.run_id(),
        engine = %engine,
        "forge-vk: voice transcription started"
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
/// Докачка идёт ТЕМ ЖЕ путём, что и `GET /api/vk/media/...`, вместе с его
/// дедупом ([`fetch_cached`]): вкладка, открывшая карточку, и кнопка
/// «расшифровать» не должны качать один файл дважды.
async fn obtain_media(state: &Arc<VkState>, key: &CacheKey) -> Result<CachedMedia, ApiError> {
    if let Some(hit) = media_cache::get(&state.config, key).await {
        // Раскладка кэша описана ещё и в ядре (`forge_voice::audio`), потому что
        // она общая у трёх плагинов. Сверка держит две половины вместе: разъедься
        // они, авторазбор Ф4 искал бы файл не там, и молча.
        debug_assert_eq!(
            forge_voice::audio::resolve_media(
                &state.config.cache_dir,
                &key.peer_id_string(),
                &cache_stem(key),
            )
            .as_deref(),
            Some(hit.path.as_path()),
            "раскладка медиа-кэша VK разошлась с раскладкой forge-voice"
        );
        return Ok(hit);
    }
    require_connected(state).await?;
    let client = require_client(state).await?;
    fetch_cached(state, key, || {
        download_attachment(state.clone(), client.clone(), *key)
    })
    .await
}

/// `GET /api/vk/voice/:id/:mid/:index/transcript`.
///
/// Ни одного вызова VK и ни одного запуска движка: ответ целиком собирается из
/// локального файла стора. Поэтому роут работает и при мёртвой сети — как
/// `GET /api/vk/dialogs/:id/analysis`.
///
/// # Errors
///
/// `400 invalid_input`, `409 not_authorized`, `500 voice_config`.
pub async fn get_transcript(
    State(state): State<Arc<VkState>>,
    Path((peer, mid, index)): Path<(String, String, String)>,
) -> Result<Json<TranscriptResp>, ApiError> {
    let key = parse_key(&peer, &mid, &index)?;
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

/// Ответ `GET`/`PUT` `/api/vk/voice/settings`.
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

/// Тело `PUT /api/vk/voice/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetVoiceSettingsReq {
    /// Движок: `local` либо `cloud`.
    pub engine: String,
}

/// `GET /api/vk/voice/settings` — какой движок выбран и всё ли для него есть.
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
    State(state): State<Arc<VkState>>,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let cfg = state.voice_config().map_err(|e| map_voice_error(&e))?;
    Ok(Json(forge_voice::settings::describe(&cfg).into()))
}

/// `PUT /api/vk/voice/settings` — переключить движок.
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
    State(state): State<Arc<VkState>>,
    Json(req): Json<SetVoiceSettingsReq>,
) -> Result<Json<VoiceSettingsResp>, ApiError> {
    let engine = forge_voice::settings::parse_engine(&req.engine).ok_or_else(|| {
        ApiError::bad_input("engine: допустимы только \"local\" и \"cloud\"")
    })?;
    let (voice_path, meet_path) = state.voice_paths();
    let settings = forge_voice::settings::set_engine(&voice_path, &meet_path, engine)
        .map_err(|e| map_voice_error(&e))?;
    tracing::info!(
        target: "forge_vk",
        engine = %engine.as_str(),
        "forge-vk: voice engine switched"
    );
    Ok(Json(settings.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SecretToken, VkConfig};
    use crate::login::{LoginFlow, VkUserDto};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use forge_voice::{Engine, Transcript};
    use tower::ServiceExt;

    /// Состояние с изолированными каталогами и заведомо мёртвой сетью.
    fn state(dir: &std::path::Path) -> Arc<VkState> {
        VkState::new(VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            base_url: "http://127.0.0.1:1/method".into(),
            cache_dir: dir.join("cache"),
            config_path: dir.join("vk.toml"),
            prompts_path: dir.join("prompts.json"),
            ..VkConfig::default()
        })
    }

    /// Пишет `voice.toml` РЯДОМ с конфигом плагина — там же, где его ищет
    /// [`VkState::voice_config`]. Никаких переменных окружения: настройки
    /// теста не имеют права утечь в соседний тест (и уж тем более к
    /// пользователю).
    ///
    /// Ключ заведомо ненастоящий: настоящий ключ polza.ai в исходниках тестов
    /// запрещён (`forge-v0m9k`).
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
        // Пустой `meet.toml`: иначе лестница дотянулась бы до настоящего
        // конфига пользователя, и тест зависел бы от его машины.
        std::fs::write(dir.join("meet.toml"), b"").unwrap();
    }

    async fn authorized(dir: &std::path::Path) -> Arc<VkState> {
        write_voice_config(dir);
        let s = state(dir);
        s.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1234".into(),
                ..Default::default()
            },
        })
        .await;
        s
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send(state: Arc<VkState>, method: &str, uri: &str) -> axum::response::Response {
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

    /// Кладёт готовую расшифровку в стор.
    fn put_transcript(state: &Arc<VkState>, text: &str) {
        let key = CacheKey::parse("2000000042", "7001", "0", Some("full")).unwrap();
        state
            .voice_store()
            .put(
                &voice_key(&key),
                &Transcript::new(text, Engine::Cloud)
                    .with_language("ru")
                    .with_duration_ms(4200),
            )
            .unwrap();
    }

    /// **ФОРМА КООРДИНАТ ПРОВЕРЯЕТСЯ ПЕРВОЙ — ДО АВТОРИЗАЦИИ.**
    ///
    /// Как у `/api/vk/media/...`: `400` не рассказывает ничего о чужих
    /// беседах, а `409` на мусорном индексе означал бы, что разбор идёт после
    /// аутентификации и её можно прощупывать.
    #[tokio::test]
    async fn the_shape_is_checked_before_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        for uri in [
            "/api/vk/voice/1/2/10/transcript",
            "/api/vk/voice/абв/2/0/transcript",
            "/api/vk/voice/1/0/0/transcript",
            "/api/vk/voice/1/2/+1/transcript",
        ] {
            let resp = send(state(dir.path()), "GET", uri).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "invalid_input", "{uri}");
        }
    }

    /// **БЕЗ АККАУНТА — `409 not_authorized`, ОБА РОУТА.**
    ///
    /// Расшифровка — это содержание личной переписки, и отдавать её без логина
    /// нельзя даже в одиночном режиме.
    #[tokio::test]
    async fn both_routes_demand_authorization() {
        let dir = tempfile::tempdir().unwrap();
        for (method, uri) in [
            ("GET", "/api/vk/voice/2000000042/7001/0/transcript"),
            ("POST", "/api/vk/voice/2000000042/7001/0/transcribe"),
        ] {
            let resp = send(state(dir.path()), method, uri).await;
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{method} {uri}");
            assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
        }
    }

    /// **РАСШИФРОВКИ НЕТ — ЭТО `200 {ready:false}`, А НЕ `404`.**
    #[tokio::test]
    async fn a_missing_transcript_is_a_normal_state() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = send(st, "GET", "/api/vk/voice/2000000042/7001/0/transcript").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["ready"], false);
        assert_eq!(json["text"], "");
        assert!(json["created_at"].is_null());
    }

    /// **ГОТОВЫЙ ТРАНСКРИПТ ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК: ОН ЧИТАЕТСЯ ИЗ ФАЙЛА.**
    ///
    /// Второе состояние собирается с нуля на том же каталоге — ровно то, что
    /// произойдёт при рестарте devforge.
    #[tokio::test]
    async fn a_stored_transcript_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = authorized(dir.path()).await;
        put_transcript(&first, "привет, это расшифровка");
        drop(first);

        let second = authorized(dir.path()).await;
        let json = body_json(send(second, "GET", "/api/vk/voice/2000000042/7001/0/transcript").await)
            .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "привет, это расшифровка");
        assert_eq!(json["language"], "ru");
        assert_eq!(json["engine"], "cloud");
        assert_eq!(json["duration_ms"], 4200);
    }

    /// **ПОВТОРНЫЙ POST НА ГОТОВОЕ НЕ ЗАПУСКАЕТ ДВИЖОК.**
    ///
    /// Сеть при этом заведомо мертва, а ffmpeg в конфиге не указан: любой
    /// запуск упёрся бы в отказ, а мы получаем текст.
    #[tokio::test]
    async fn a_repeated_post_returns_the_ready_text_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        put_transcript(&st, "уже готово");

        let resp = send(st.clone(), "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "уже готово");
        assert!(json["run_id"].is_null(), "движок не запускался");
        assert_eq!(st.voice.active(), 0);
    }

    /// **НЕТ ВЛОЖЕНИЯ И НЕТ СЕТИ — `503`, А НЕ `500` И НЕ ПАНИКА.**
    #[tokio::test]
    async fn a_missing_attachment_without_a_connection_is_503() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        st.note_down(crate::dto::now_unix(), "connection reset").await;

        let resp = send(st, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_json(resp).await["error"]["code"], "not_connected");
    }

    /// **НЕНАСТРОЕННОЕ ОБЛАКО — `409 voice_no_key`, А НЕ `500` И НЕ ПОХОД В
    /// СЕТЬ.**
    ///
    /// Отказ приходит ДО скачивания вложения: качать мегабайты, чтобы потом
    /// сказать «ключа нет», — худший из возможных порядков.
    #[tokio::test]
    async fn an_unconfigured_engine_is_refused_before_any_download() {
        let dir = tempfile::tempdir().unwrap();
        // Конфига голосовых нет вовсе: ни `voice.toml`, ни `meet.toml`.
        let st = state(dir.path());
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1234".into(),
                ..Default::default()
            },
        })
        .await;

        let resp = send(st.clone(), "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_no_key");
        assert!(
            !json["error"]["message"].as_str().unwrap().contains("pza_"),
            "ключ не должен светиться даже маской: {json}"
        );
        assert_eq!(st.voice.active(), 0, "прогон не запускался");
    }

    /// **БИТЫЙ `voice.toml` — ВНЯТНЫЙ ОТКАЗ, А НЕ 500 БЕЗ ОБЪЯСНЕНИЙ.**
    ///
    /// И готовый транскрипт при этом всё равно отдаётся: он лежит в файле и от
    /// настроек не зависит.
    #[tokio::test]
    async fn a_broken_config_is_a_named_error_and_does_not_hide_ready_text() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        std::fs::write(dir.path().join("voice.toml"), "это = не = toml").unwrap();

        let resp = send(st.clone(), "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "voice_config");
        assert!(
            json["error"]["message"].as_str().unwrap().contains("voice.toml"),
            "человеку надо знать, какой файл чинить: {json}"
        );

        // А готовое — по-прежнему отдаётся.
        put_transcript(&st, "посчитано раньше поломки");
        let json = body_json(
            send(st, "POST", "/api/vk/voice/2000000042/7001/0/transcribe").await,
        )
        .await;
        assert_eq!(json["ready"], true);
        assert_eq!(json["text"], "посчитано раньше поломки");
    }

    /// Шлёт запрос с телом JSON.
    async fn send_json(
        state: Arc<VkState>,
        method: &str,
        uri: &str,
        body: &str,
    ) -> axum::response::Response {
        crate::routes::build_router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// **НАСТРОЙКИ ДВИЖКА ЧИТАЮТСЯ БЕЗ ЛОГИНА И БЕЗ КЛЮЧА В ОТВЕТЕ.**
    ///
    /// Без логина — намеренно: вкладка настроек открывается до входа в аккаунт,
    /// и «войдите в VK, чтобы выбрать движок расшифровки» было бы абсурдом.
    #[tokio::test]
    async fn the_engine_is_readable_without_a_login_and_never_carries_the_key() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let resp = send(state(dir.path()), "GET", "/api/vk/voice/settings").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["engine"], "cloud");
        assert_eq!(json["cloud_key_set"], true);
        assert!(!json.to_string().contains("pza_"), "ключ не отдаётся: {json}");
    }

    /// **ПЕРЕКЛЮЧЕНИЕ ДВИЖКА ПИШЕТСЯ В `voice.toml` И ДЕЙСТВУЕТ СРАЗУ.**
    ///
    /// «Сразу» проверяется следующим `GET`-ом по тому же состоянию: конфиг
    /// перечитывается на каждый запрос, поэтому перезапуск devforge не нужен.
    /// Комментарии в файле при этом целы — его правят руками.
    #[tokio::test]
    async fn switching_the_engine_lands_in_the_file_and_takes_effect_at_once() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        std::fs::write(
            dir.path().join("voice.toml"),
            "# комментарий человека\nengine = \"cloud\"\n\
             polza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        let st = state(dir.path());

        let resp = send_json(
            st.clone(),
            "PUT",
            "/api/vk/voice/settings",
            r#"{"engine":"local"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["engine"], "local");

        let raw = std::fs::read_to_string(dir.path().join("voice.toml")).unwrap();
        assert!(raw.contains("# комментарий человека"), "{raw}");
        assert!(raw.contains("engine = \"local\""), "{raw}");

        let json = body_json(send(st, "GET", "/api/vk/voice/settings").await).await;
        assert_eq!(json["engine"], "local", "без перезапуска процесса");
    }

    /// **ЧУЖОЕ СЛОВО ДВИЖКА — `400`, И ФАЙЛ НЕ ТРОНУТ.**
    #[tokio::test]
    async fn an_unknown_engine_is_refused_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        let before = std::fs::read_to_string(dir.path().join("voice.toml")).unwrap();

        for body in [r#"{"engine":"openai"}"#, r#"{"engine":""}"#] {
            let resp = send_json(state(dir.path()), "PUT", "/api/vk/voice/settings", body).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body_json(resp).await["error"]["code"], "invalid_input");
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("voice.toml")).unwrap(),
            before
        );
    }

    /// **ВЫБОР СОХРАНЯЕТСЯ, ДАЖЕ ЕСЛИ ДВИЖОК СЕЙЧАС НЕРАБОЧИЙ, — С
    /// ПРЕДУПРЕЖДЕНИЕМ.**
    ///
    /// Локальный движок без бинаря whisper: ответ `200`, в файле `local`, а в
    /// теле — код, по которому UI объяснит, чего не хватает.
    #[tokio::test]
    async fn an_unusable_engine_is_still_saved_but_warns() {
        let dir = tempfile::tempdir().unwrap();
        write_voice_config(dir.path());
        // Пути бинарей заведомо мимо: whisper искать негде.
        std::fs::write(
            dir.path().join("voice.toml"),
            format!(
                "engine = \"cloud\"\nwhisper_path = {:?}\nffmpeg_path = {:?}\n",
                dir.path().join("no-whisper").display().to_string(),
                dir.path().join("ffmpeg-stub").display().to_string(),
            ),
        )
        .unwrap();

        let resp = send_json(
            state(dir.path()),
            "PUT",
            "/api/vk/voice/settings",
            r#"{"engine":"local"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["engine"], "local");
        assert_eq!(json["warning"]["code"], "voice_no_whisper");
        assert!(std::fs::read_to_string(dir.path().join("voice.toml"))
            .unwrap()
            .contains("engine = \"local\""));
    }

    /// Разбор `?force=1` — ручной, как и вся query в плагине.
    #[test]
    fn the_force_flag_is_parsed_by_hand() {
        assert!(force_requested(Some("force=1")));
        assert!(force_requested(Some("x=1&force=true")));
        assert!(!force_requested(Some("force=0")));
        assert!(!force_requested(Some("forced=1")));
        assert!(!force_requested(None));
    }
}
