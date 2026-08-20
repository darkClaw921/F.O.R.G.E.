//! Локальный стенд, изображающий `api.polza.ai`, — флагманский тест облачного
//! клиента (Ф14).
//!
//! Секретов не требует, наружу не ходит и гоняет ВЕСЬ транспорт через
//! настоящий `reqwest`: заголовок авторизации, разбор ответов, форму
//! `multipart`, нарезку записи на окна, поток SSE и все три способа отказать.
//!
//! # Почему свой стенд, а не `wiremock`
//!
//! Решение задачи `forge-meet-f14-polza-client-8vxxg.5`: новых dev-зависимостей
//! крейт не заводит. Стенд собирается из уже имеющихся `axum` +
//! `tokio::net::TcpListener::bind("127.0.0.1:0")`, а точкой подмены служит
//! `CloudConfig::base_url` — ровно как `VkConfig::base_url` в
//! `plugins/vk/tests/vk_stub.rs`. Прямая dev-зависимость на wiremock вдобавок
//! сделала бы ложным объяснение в шапке `plugins/meet/Cargo.toml`, по которому
//! `regex` не подключается напрямую именно потому, что живёт в дереве
//! транзитивом wiremock.
//!
//! # Три способа отказать, и каждый нужен
//!
//! | отказ | что проверяет |
//! |---|---|
//! | [`Reply::Status`] | текст отказа доходит до пользователя, а не теряется |
//! | [`Reply::Drop`] | обрыв соединения — то, чем живой polza.ai отвечает на слишком длинное аудио |
//! | [`Reply::Hang`] | свой таймаут срабатывает и не ждёт вечно |
//!
//! # Главный трюк стенда
//!
//! Мок ffmpeg (`mock_ffmpeg_window_slicer`) пишет в «mp3» строку
//! `ID3mock ss=<начало> t=<длительность>`, то есть окно, которое у него
//! попросили. Стенд читает эти значения из принятого файла и РВЁТ СОЕДИНЕНИЕ на
//! куске длиннее 90 секунд — так же, как живой сервис рвёт TCP на 62-й секунде.
//! Поэтому тест доказывает, что запись режется ДО отправки, а не после отказа.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use forge_meet::cloud::service::{filter_kind, KIND_CHAT, KIND_STT};
use forge_meet::cloud::stt::{self, SttOptions};
use forge_meet::cloud::summary::ChatOptions;
use forge_meet::cloud::{CloudClient, CloudConfig, SecretKey};
use forge_meet::summary::BuiltPrompt;
use forge_meet::testing::mocks;

/// Ключ, который стенд обязан увидеть в ЗАГОЛОВКЕ и не имеет права увидеть
/// больше нигде.
const KEY: &str = "pza_TESTnotArealKeyJustAFixture_mask01";

// ===========================================================================
// Стенд
// ===========================================================================

/// Что стенд ответит на очередной вызов.
#[derive(Debug, Clone)]
enum Reply {
    /// Успех с телом.
    Json(Value),
    /// Отказ с телом — текст обязан доехать до человека.
    Status(u16, String),
    /// Поток SSE: кадры уезжают кусками по нескольку байт, чтобы граница
    /// чтения гарантированно разрезала и кадр, и символ.
    Sse(Vec<String>),
    /// Поток, который обрывается на середине.
    SseThenDrop(Vec<String>),
    /// Поток, который замолкает на середине и больше ничего не шлёт.
    SseThenQuiet(Vec<String>, u64),
    /// Обрыв соединения.
    Drop,
    /// Молчание дольше любого разумного таймаута.
    Hang(u64),
}

#[derive(Debug, Clone)]
struct Call {
    path: String,
    query: String,
    /// Значение `Authorization`.
    auth: Option<String>,
    /// Все заголовки — по ним проверяется, что ключа нет больше нигде.
    headers: Vec<(String, String)>,
    /// Тело как текст (для JSON-запросов).
    body_text: String,
    /// Текстовые поля формы.
    fields: BTreeMap<String, String>,
    /// Имя присланного файла.
    file_name: Option<String>,
    /// Его `Content-Type`.
    file_type: Option<String>,
    /// Его содержимое.
    file_body: String,
}

impl Call {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body_text).unwrap_or(Value::Null)
    }

    /// `ss` и `t`, которые мок ffmpeg записал в «mp3».
    fn window(&self) -> Option<(f64, f64)> {
        let ss = self.file_body.split("ss=").nth(1)?.split_whitespace().next()?;
        let t = self.file_body.split("t=").nth(1)?.split_whitespace().next()?;
        Some((ss.parse().ok()?, t.parse().ok()?))
    }
}

#[derive(Default)]
struct StubState {
    calls: Mutex<Vec<Call>>,
    /// Путь → очередь заготовленных ответов. Пусто ⇒ разумный дефолт.
    script: Mutex<HashMap<String, VecDeque<Reply>>>,
    /// Окна (по `ss`), на которых стенд обязан оборваться.
    fail_windows: Mutex<HashSet<String>>,
}

struct Stub {
    base_url: String,
    state: Arc<StubState>,
}

impl Stub {
    fn script(&self, path: &str, reply: Reply) {
        self.state
            .script
            .lock()
            .unwrap()
            .entry(path.to_string())
            .or_default()
            .push_back(reply);
    }

    fn calls(&self) -> Vec<Call> {
        self.state.calls.lock().unwrap().clone()
    }

    fn calls_to(&self, path: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|c| c.path == path).collect()
    }

    fn fail_window(&self, ss: &str) {
        self.state
            .fail_windows
            .lock()
            .unwrap()
            .insert(ss.to_string());
    }

    fn client(&self) -> CloudClient {
        CloudClient::new(&self.config()).unwrap()
    }

    fn config(&self) -> CloudConfig {
        CloudConfig {
            base_url: self.base_url.clone(),
            api_key: SecretKey::new(KEY),
            ..CloudConfig::default()
        }
    }
}

/// Поднимает стенд на свободном порту. Жёсткий порт однажды упёрся бы в живой
/// devforge на 7331.
async fn start_stub() -> Stub {
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .fallback(handle_any)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Stub {
        base_url: format!("http://{addr}/api/v1"),
        state,
    }
}

async fn handle_any(
    State(state): State<Arc<StubState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (fields, file_name, file_type, file_body) = if content_type.starts_with("multipart/") {
        parse_multipart(&content_type, &body)
    } else {
        (BTreeMap::new(), None, None, String::new())
    };

    let call = Call {
        path: path.clone(),
        query: uri.query().unwrap_or("").to_string(),
        auth: headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<bin>").to_string()))
            .collect(),
        body_text: if content_type.starts_with("multipart/") {
            String::new()
        } else {
            String::from_utf8_lossy(&body).to_string()
        },
        fields,
        file_name,
        file_type,
        file_body,
    };
    state.calls.lock().unwrap().push(call.clone());

    let scripted = state
        .script
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front);
    if let Some(r) = scripted {
        return reply(r).await;
    }

    // Дефолты: скучный успех в живой форме ответа.
    match path.as_str() {
        "/api/v1/key" => axum::Json(live_key()).into_response(),
        "/api/v1/balance" => axum::Json(live_balance()).into_response(),
        "/api/v1/models" => axum::Json(live_models()).into_response(),
        "/api/v1/audio/transcriptions" => transcription_reply(&state, &call).await,
        "/api/v1/chat/completions" => {
            reply(Reply::Sse(chat_frames(&["Итоги", " встречи."]))).await
        }
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

/// Ответ на присланное окно.
///
/// Здесь и живёт главный трюк стенда: кусок длиннее 90 секунд обрывает
/// соединение, как живой сервис на 62-й секунде стены.
async fn transcription_reply(state: &Arc<StubState>, call: &Call) -> Response {
    let Some((ss, t)) = call.window() else {
        return (StatusCode::BAD_REQUEST, "стенд не понял присланный файл").into_response();
    };
    if t > 90.5 {
        return broken_response();
    }
    let ss_key = format!("{ss:.3}");
    if state.fail_windows.lock().unwrap().contains(&ss_key) {
        return broken_response();
    }
    // Две реплики: в начале окна и в его конце — по ним видно и смещение, и
    // то, что стык окон не потерялся.
    axum::Json(json!({
        "task": "transcribe",
        "language": "russian",
        "duration": t,
        "text": format!("начало {ss} конец {ss}"),
        "segments": [
            {"id": 0, "start": 0.0, "end": 1.0, "text": format!(" начало {ss}"),
             "tokens": [50364, 1770], "no_speech_prob": 0.01},
            {"id": 1, "start": t - 1.0, "end": t, "text": format!(" конец {ss}"),
             "tokens": [50364, 1771], "no_speech_prob": 0.01}
        ]
    }))
    .into_response()
}

async fn reply(r: Reply) -> Response {
    match r {
        Reply::Json(v) => axum::Json(v).into_response(),
        Reply::Status(code, body) => (
            StatusCode::from_u16(code).unwrap(),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Reply::Drop => broken_response(),
        Reply::Hang(secs) => {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            axum::Json(json!({"late": true})).into_response()
        }
        Reply::Sse(frames) => sse_response(frames, Tail::Close),
        Reply::SseThenDrop(frames) => sse_response(frames, Tail::Drop),
        Reply::SseThenQuiet(frames, secs) => sse_response(frames, Tail::Quiet(secs)),
    }
}

/// Чем кончается поток стенда.
enum Tail {
    /// Штатное закрытие.
    Close,
    /// Обрыв соединения.
    Drop,
    /// Молчание — соединение живо, кадров больше нет.
    Quiet(u64),
}

/// Ответ с оборванным телом: заголовки доезжают, тело — нет.
fn broken_response() -> Response {
    let stream = futures_util::stream::once(async {
        Err::<Vec<u8>, std::io::Error>(std::io::Error::other("stub: connection dropped"))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::CONTENT_LENGTH, "4096")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Поток SSE, нарезанный на куски по семь байт.
///
/// Семь, а не «по кадру»: граница чтения в жизни не совпадает ни с границей
/// кадра, ни с границей символа, и разборщик обязан это переживать.
fn sse_response(frames: Vec<String>, tail: Tail) -> Response {
    use futures_util::StreamExt;

    let text = frames.join("");
    let chunks: Vec<Vec<u8>> = text.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
    // Пауза между кусками не украшение: без неё маленький ответ уезжает одним
    // куском вместе с ошибкой хвоста, и «обрыв на середине» превращается в
    // «запрос не отправился» — то есть проверялся бы не тот случай.
    let ok = futures_util::stream::iter(chunks.into_iter().map(Ok::<Vec<u8>, std::io::Error>))
        .then(|item| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            item
        });
    let body = match tail {
        Tail::Close => Body::from_stream(ok),
        // Пауза перед обрывом: без неё ошибка догоняет последний кусок в
        // буфере записи, и клиент не успевает увидеть уже отправленный кадр —
        // стенд проверял бы «ответ вообще не пришёл» вместо «оборвался на
        // середине».
        Tail::Drop => Body::from_stream(ok.chain(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err::<Vec<u8>, std::io::Error>(std::io::Error::other("stub: stream died midway"))
        }))),
        Tail::Quiet(secs) => Body::from_stream(ok.chain(futures_util::stream::once(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            Ok::<Vec<u8>, std::io::Error>(Vec::new())
        }))),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

/// Кадры OpenAI-вида плюс завершающий `[DONE]`.
fn chat_frames(parts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = parts
        .iter()
        .map(|p| {
            format!(
                "data: {}\n\n",
                json!({"provider": "google-vertex/global",
                       "choices": [{"delta": {"content": p}}]})
            )
        })
        .collect();
    out.push(": keep-alive\n\n".to_string());
    out.push("data: [DONE]\n\n".to_string());
    out
}

/// Разбор `multipart/form-data` — ровно столько, сколько нужно стенду.
fn parse_multipart(
    content_type: &str,
    body: &[u8],
) -> (BTreeMap<String, String>, Option<String>, Option<String>, String) {
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .unwrap_or("")
        .trim_matches('"')
        .to_string();
    let mut fields = BTreeMap::new();
    let (mut file_name, mut file_type, mut file_body) = (None, None, String::new());
    if boundary.is_empty() {
        return (fields, file_name, file_type, file_body);
    }
    let raw = String::from_utf8_lossy(body).to_string();
    for part in raw.split(&format!("--{boundary}")) {
        let part = part.trim_start_matches("\r\n");
        let Some((head, data)) = part.split_once("\r\n\r\n") else {
            continue;
        };
        let name = head
            .split("name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let data = data.trim_end_matches("\r\n").trim_end_matches("--").to_string();
        if let Some(fname) = head.split("filename=\"").nth(1).and_then(|s| s.split('"').next()) {
            file_name = Some(fname.to_string());
            file_type = head
                .split("Content-Type: ")
                .nth(1)
                .and_then(|s| s.split("\r\n").next())
                .map(str::to_string);
            file_body = data;
        } else {
            fields.insert(name, data);
        }
    }
    (fields, file_name, file_type, file_body)
}

// --- живые формы ответов ------------------------------------------------------

fn live_key() -> Value {
    json!({
        "creator_user_id": "usr_2109015473186291713",
        "label": "pza_RIcg••••••••••••••••••••Uzu4js",
        "limit": 300, "limit_remaining": 288.7275241, "limit_reset": "monthly",
        "usage": 11.2724759
    })
}

fn live_balance() -> Value {
    json!({
        "amount": "461.88706171", "reservedAmount": "0.00000000",
        "spentAmount": "138.72166217", "updatedAt": "2026-08-18T22:27:57.610Z"
    })
}

fn live_models() -> Value {
    json!({"object": "list", "data": [
        {"id": "openai/whisper-1", "name": "Whisper 1", "type": "stt", "pricing": null,
         "top_provider": {"context_length": 0, "max_completion_tokens": null,
             "pricing": {"stt_per_minute": "0.43200000", "currency": "RUB"}}},
        {"id": "google/gemini-3.5-flash", "name": "Gemini 3.5 Flash", "type": "chat",
         "pricing": null,
         "top_provider": {"context_length": 1048576, "max_completion_tokens": 65536,
             "pricing": {"prompt_per_million": "176.05000000",
                         "completion_per_million": "1056.32000000", "currency": "RUB"}}},
        {"id": "openai/tts-1", "type": "tts", "pricing": null, "top_provider": {}}
    ]})
}

// --- вспомогательное ----------------------------------------------------------

fn short() -> Duration {
    Duration::from_millis(1_500)
}

/// Каталог встречи с «записью» и моком ffmpeg-нарезчика.
fn stand(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let wav = dir.join("work/mix.wav");
    std::fs::create_dir_all(wav.parent().unwrap()).unwrap();
    std::fs::write(&wav, b"RIFF....WAVE").unwrap();
    let ffmpeg = mocks::mock_ffmpeg_window_slicer(&dir.join("bin"), "ffmpeg").unwrap();
    (wav, ffmpeg)
}

fn stt_options(ffmpeg: &std::path::Path, window_secs: u64) -> SttOptions {
    SttOptions {
        ffmpeg: ffmpeg.to_path_buf(),
        ffmpeg_timeout: Duration::from_secs(10),
        window_ms: window_secs as i64 * 1_000,
        model: forge_meet::cloud::DEFAULT_STT_MODEL.to_string(),
        language: "ru".to_string(),
        request_timeout: short(),
        cancel: CancellationToken::new(),
        // Пусто — арифметическая нарезка. Тесты, которым нужна нарезка по
        // тишине, кладут интервалы сами.
        speech: Vec::new(),
    }
}

// ===========================================================================
// Служебные вызовы
// ===========================================================================

#[tokio::test]
async fn the_service_endpoints_are_read_from_the_live_shapes() {
    let stub = start_stub().await;
    let c = stub.client();

    let key = c.key_info(short()).await.unwrap();
    assert_eq!(key.limit_remaining, Some(288.7275241));
    assert_eq!(key.limit_reset.as_deref(), Some("monthly"));
    assert_eq!(key.exhausted(), Some(false));

    let b = c.balance(short()).await.unwrap();
    assert!((b.amount - 461.88706171).abs() < 1e-8, "{b:?}");
    assert_eq!(b.currency, "RUB");

    let models = c.models(short()).await.unwrap();
    assert_eq!(models.len(), 3);
    let chat = filter_kind(&models, KIND_CHAT);
    assert_eq!(chat.len(), 1);
    let p = chat[0].pricing.as_ref().expect("цена лежит в top_provider");
    assert_eq!(p.prompt_per_million, Some(176.05));
    assert_eq!(filter_kind(&models, KIND_STT).len(), 1);

    // Адреса именно те, что у живого API.
    let paths: Vec<String> = stub.calls().into_iter().map(|c| c.path).collect();
    assert_eq!(
        paths,
        vec!["/api/v1/key", "/api/v1/balance", "/api/v1/models"]
    );
}

/// **КЛЮЧ УХОДИТ ТОЛЬКО ЗАГОЛОВКОМ.**
///
/// Ни в query (он попадает в логи прокси и в текст ошибок), ни в теле, ни в
/// каком-либо другом заголовке.
#[tokio::test]
async fn the_api_key_travels_only_in_the_authorization_header() {
    let stub = start_stub().await;
    let c = stub.client();
    let _ = c.key_info(short()).await.unwrap();
    let _ = c.models(short()).await.unwrap();

    for call in stub.calls() {
        assert_eq!(call.auth.as_deref(), Some(&format!("Bearer {KEY}")[..]));
        assert!(call.query.is_empty(), "ключ мог уехать в query: {call:?}");
        assert!(!call.body_text.contains(KEY), "ключ в теле: {call:?}");
        for (name, value) in &call.headers {
            if name.eq_ignore_ascii_case("authorization") {
                continue;
            }
            assert!(
                !value.contains(KEY),
                "ключ утёк в заголовок {name}: {value}"
            );
        }
    }
}

/// Текст отказа доезжает до пользователя, а не превращается в «внутренняя
/// ошибка».
#[tokio::test]
async fn an_error_body_reaches_the_caller() {
    let stub = start_stub().await;
    stub.script(
        "/api/v1/key",
        Reply::Status(
            401,
            r#"{"error":{"code":"UNAUTHORIZED","message":"Неверные учётные данные авторизации","trace_id":"e937"}}"#
                .to_string(),
        ),
    );
    let err = stub.client().key_info(short()).await.unwrap_err();
    assert_eq!(err.code(), "cloud_unauthorized");
    assert!(
        err.message().contains("Неверные учётные данные"),
        "текст отказа потерян: {err}"
    );
    assert!(!err.to_string().contains(KEY), "ключ в тексте ошибки: {err}");
}

/// Ответ без лимитов — это «не знаю», а не «лимит исчерпан».
///
/// Разница видна пользователю: ноль в остатке заставил бы его идти пополнять
/// счёт, которого хватает.
#[tokio::test]
async fn a_key_response_without_limits_is_not_an_exhausted_key() {
    let stub = start_stub().await;
    stub.script(
        "/api/v1/key",
        Reply::Json(json!({"label": "pza_RIcg••••Uzu4js"})),
    );
    let key = stub.client().key_info(short()).await.unwrap();
    assert_eq!(key.limit_remaining, None);
    assert_eq!(key.exhausted(), None, "молчание принято за исчерпанный лимит");
}

#[tokio::test]
async fn a_silent_endpoint_is_cut_by_the_call_timeout() {
    let stub = start_stub().await;
    stub.script("/api/v1/balance", Reply::Hang(30));
    let started = std::time::Instant::now();
    let err = stub
        .client()
        .balance(Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cloud_timeout", "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "ждали вечно");
}

#[tokio::test]
async fn a_dropped_connection_is_a_transport_error() {
    let stub = start_stub().await;
    stub.script("/api/v1/models", Reply::Drop);
    let err = stub.client().models(short()).await.unwrap_err();
    assert!(
        matches!(err.code(), "cloud_transport" | "cloud_bad_response"),
        "{err}"
    );
    assert!(err.retriable() || err.code() == "cloud_bad_response");
}

// ===========================================================================
// Расшифровка
// ===========================================================================

/// **ГЛАВНЫЙ ТЕСТ ФАЗЫ.** Длинная запись режется на окна, а таймкоды
/// возвращаются на ОБЩУЮ ось без разрыва на стыке.
#[tokio::test]
async fn a_long_recording_is_cut_into_windows_and_stitched_onto_one_axis() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;

    // Пять минут записи окнами по 90 секунд: четыре окна, последнее короче.
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = seen.clone();
    let on_progress = move |p: u8| sink.lock().unwrap().push(p);
    let out = stt::transcribe_file(
        &stub.client(),
        &wav,
        300_000,
        &dir.path().join("work"),
        &stt_options(&ffmpeg, 90),
        Some(&on_progress),
    )
    .await
    .unwrap();

    assert_eq!(out.windows, 4);
    assert!(out.complete(), "{:?}", out.failed);
    assert_eq!(out.language.as_deref(), Some("russian"));
    assert_eq!(*seen.lock().unwrap(), vec![25, 50, 75, 100], "прогресс по окнам");

    // Запрошенные у ffmpeg окна: встык, последнее — хвост.
    let asked: Vec<(f64, f64)> = stub
        .calls_to("/api/v1/audio/transcriptions")
        .iter()
        .map(|c| c.window().unwrap())
        .collect();
    assert_eq!(asked, vec![(0.0, 90.0), (90.0, 90.0), (180.0, 90.0), (270.0, 30.0)]);

    // И самое важное: сегменты лежат на ОДНОЙ оси, стык окон непрерывен.
    let starts: Vec<i64> = out.segments.iter().map(|s| s.start_ms).collect();
    assert_eq!(
        starts,
        vec![0, 89_000, 90_000, 179_000, 180_000, 269_000, 270_000, 299_000]
    );
    assert!(
        out.segments.windows(2).all(|p| p[0].start_ms <= p[1].start_ms),
        "ось не монотонна: {starts:?}"
    );
    // Первый сегмент второго окна пришёл из ответа с локальным нулём — если бы
    // смещение не прибавилось, он лёг бы в начало встречи.
    assert_eq!(out.segments[2].text, "начало 90");
    assert_eq!(out.segments[2].end_ms, 91_000);
    // Пословных меток у облака нет: разрезать сегмент по словам нечем.
    assert!(out.segments.iter().all(|s| s.tokens.is_empty()));
}

/// **ЗАПИСЬ РЕЖЕТСЯ ДО ОТПРАВКИ, А НЕ ПОСЛЕ ОТКАЗА.**
///
/// Стенд рвёт соединение на куске длиннее 90 секунд — так живой сервис
/// отвечает на трёхминутное аудио. Конфиг при этом просит окно в десять минут:
/// если бы потолок не применялся, каждое окно оборвалось бы, и расшифровки не
/// было бы вовсе.
#[tokio::test]
async fn the_window_ceiling_is_applied_before_sending_anything() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;

    let out = stt::transcribe_file(
        &stub.client(),
        &wav,
        200_000,
        &dir.path().join("work"),
        &stt_options(&ffmpeg, 600),
        None,
    )
    .await
    .unwrap();

    assert!(out.complete(), "стенд оборвал окно: {:?}", out.failed);
    let asked = stub.calls_to("/api/v1/audio/transcriptions");
    assert_eq!(asked.len(), 3);
    assert!(
        asked.iter().all(|c| c.window().unwrap().1 <= 90.0),
        "на облако ушёл кусок длиннее измеренного предела"
    );
}

/// На облако уходит MP3, и форма запроса та, что принимает polza.ai.
#[tokio::test]
async fn the_upload_is_an_mp3_and_never_asks_for_word_timestamps() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;

    stt::transcribe_file(
        &stub.client(),
        &wav,
        60_000,
        &dir.path().join("work"),
        &stt_options(&ffmpeg, 90),
        None,
    )
    .await
    .unwrap();

    let call = &stub.calls_to("/api/v1/audio/transcriptions")[0];
    assert_eq!(call.file_name.as_deref(), Some("window-0000.mp3"));
    assert_eq!(call.file_type.as_deref(), Some("audio/mpeg"));
    assert!(
        call.file_body.starts_with("ID3"),
        "на облако ушёл не mp3: {}",
        call.file_body
    );
    assert!(
        !call.file_body.starts_with("RIFF"),
        "на облако ушёл сырой WAV — он роняет их шлюз"
    );
    assert_eq!(call.fields.get("response_format").map(String::as_str), Some("verbose_json"));
    assert_eq!(
        call.fields.get("model").map(String::as_str),
        Some("openai/whisper-1")
    );
    assert_eq!(call.fields.get("language").map(String::as_str), Some("ru"));
    assert!(
        !call.fields.keys().any(|k| k.contains("timestamp")),
        "в форме появились пословные метки: {:?}",
        call.fields
    );
    assert!(!call.file_body.contains(KEY));

    // Временные mp3 после прогона не остаются.
    let left = std::fs::read_dir(dir.path().join("work/cloud"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "окна остались лежать в каталоге встречи");
}

/// **ОДНО УПАВШЕЕ ОКНО НЕ ТЕРЯЕТ ОСТАЛЬНЫЕ И НЕ ТЕРЯЕТ ЗАПИСЬ.**
#[tokio::test]
async fn one_broken_window_leaves_a_gap_but_keeps_the_rest() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;
    stub.fail_window("90.000");

    let out = stt::transcribe_file(
        &stub.client(),
        &wav,
        300_000,
        &dir.path().join("work"),
        &stt_options(&ffmpeg, 90),
        None,
    )
    .await
    .unwrap();

    assert_eq!(out.windows, 4);
    assert!(!out.complete());
    assert_eq!(out.failed.len(), 1);
    assert_eq!(out.failed[0].index, 1);
    assert_eq!(out.failed[0].start_ms, 90_000);
    // Остальные три окна доехали целиком.
    assert_eq!(out.segments.len(), 6);
    assert!(out.segments.iter().all(|s| !(90_000..180_000).contains(&s.start_ms)));
    let note = out.gap_note().unwrap();
    assert!(note.contains("90 с"), "{note}");
    assert!(note.contains("из 4"), "{note}");
}

/// А вот когда не доехало НИ ОДНО окно — это отказ расшифровки.
#[tokio::test]
async fn a_transcription_where_nothing_arrives_is_an_error() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;
    for ss in ["0.000", "90.000"] {
        stub.fail_window(ss);
    }
    let err = stt::transcribe_file(
        &stub.client(),
        &wav,
        120_000,
        &dir.path().join("work"),
        &stt_options(&ffmpeg, 90),
        None,
    )
    .await
    .unwrap_err();
    assert!(
        err.code.starts_with("cloud_"),
        "{}",
        err.full_message()
    );
}

/// Отмена встречи снимает расшифровку, не дожидаясь оставшихся окон.
#[tokio::test]
async fn cancelling_stops_the_transcription() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;

    let opts = stt_options(&ffmpeg, 90);
    opts.cancel.cancel();
    let err = stt::transcribe_file(
        &stub.client(),
        &wav,
        300_000,
        &dir.path().join("work"),
        &opts,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "cancelled");
    assert!(
        stub.calls_to("/api/v1/audio/transcriptions").is_empty(),
        "отменённая расшифровка всё равно потратила деньги"
    );
}

/// **ОКНА МИКСА ПРОСЯТ У FFMPEG ГРАНИЦЫ, ЛЕЖАЩИЕ В ТИШИНЕ.**
///
/// `forge-12532` живьём: реплика 169–183,2 с пересекала арифметическую границу
/// 180 с, и её хвост попал в транскрипт дважды — модель договорила оборванную
/// фразу, а следующее окно расшифровало настоящий хвост. Здесь видно, что до
/// отправки дело доходит уже с другой границей.
#[tokio::test]
async fn mix_windows_are_cut_where_nobody_is_speaking() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let (wav, ffmpeg) = stand(dir.path());
    let stub = start_stub().await;

    let mut opts = stt_options(&ffmpeg, 90);
    opts.speech = vec![
        (5_000, 60_000),
        (70_000, 120_000),
        (130_000, 169_000),
        // Та самая реплика через границу 180 с.
        (169_000, 183_200),
        (190_000, 240_000),
    ];
    let out = stt::transcribe_file(&stub.client(), &wav, 300_000, &dir.path().join("work"), &opts, None)
        .await
        .unwrap();

    assert!(out.complete(), "{:?}", out.failed);
    assert!(out.forced_cuts.is_empty(), "{:?}", out.forced_cuts);
    let asked: Vec<(f64, f64)> = stub
        .calls_to("/api/v1/audio/transcriptions")
        .iter()
        .map(|c| c.window().unwrap())
        .collect();
    // Ни одна граница не разрезает реплику, и потолок 90 с не превышен.
    let edges: Vec<f64> = asked.iter().map(|(ss, t)| ss + t).collect();
    for e in &edges {
        let ms = (e * 1000.0).round() as i64;
        for (s, en) in &opts.speech {
            assert!(ms <= *s || ms >= *en, "граница {ms} разрезала реплику {s}-{en}");
        }
    }
    assert!(asked.iter().all(|(_, t)| *t <= 90.0), "{asked:?}");
    // Стык окон остаётся непрерывным: терять звук нельзя.
    assert_eq!(asked[0].0, 0.0);
    for pair in asked.windows(2) {
        assert!((pair[0].0 + pair[0].1 - pair[1].0).abs() < 0.002, "{pair:?}");
    }
    assert!((edges.last().unwrap() - 300.0).abs() < 0.002, "{edges:?}");
}

// ===========================================================================
// Расшифровка ПО ДОРОЖКАМ
// ===========================================================================

/// Дорожка на стенде: настоящий WAV в `raw/` и интервалы её речи.
fn track_stand(
    dir: &std::path::Path,
    idx: usize,
    label: &str,
    total_ms: i64,
    speech: &[(i64, i64)],
) -> forge_meet::pipeline::pertrack::TrackJob {
    use forge_meet::ingest::wav::WavWriter;

    let raw = dir.join("raw");
    std::fs::create_dir_all(&raw).unwrap();
    let wav = raw.join(format!("track-{idx}.wav"));
    let mut w = WavWriter::create(&wav, 16_000).unwrap();
    // Ровно столько сэмплов, сколько миллисекунд у записи: вырезка режет по
    // сэмплам, и короткий файл молча укоротил бы куски.
    w.write_frame(0, &vec![1_000i16; 16 * total_ms as usize]).unwrap();
    w.finish().unwrap();

    forge_meet::pipeline::pertrack::TrackJob {
        idx,
        label: label.to_string(),
        wav,
        speech: speech.to_vec(),
    }
}

fn track_request(dir: &std::path::Path, total_ms: i64, pad_ms: i64) -> stt::CloudTrackRequest {
    stt::CloudTrackRequest {
        work_dir: dir.join("work"),
        sample_rate: 16_000,
        total_ms,
        pad_ms,
    }
}

/// **ИМЯ СПИКЕРА СТАНОВИТСЯ ФАКТОМ, А НЕ ДОГАДКОЙ.**
///
/// Главный тест Ф18. По миксу спикер вычисляется перекрытием сегмента с
/// интервалами речи, и на стыках реплик это даёт ошибки (замер приёмки Ф17:
/// 32-34 верных имени из 43). Здесь дорожка И ЕСТЬ человек: у каждого сегмента
/// подпись владельца дорожки, чужих имён нет ни одного.
#[tokio::test]
async fn every_cloud_segment_carries_the_name_of_the_track_it_came_from() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = mocks::mock_ffmpeg_window_slicer(&dir.path().join("bin"), "ffmpeg").unwrap();
    let stub = start_stub().await;

    // Две дорожки с НЕПЕРЕСЕКАЮЩЕЙСЯ речью на записи в 30 секунд.
    let jobs = vec![
        track_stand(dir.path(), 0, "Аня", 30_000, &[(0, 5_000), (20_000, 25_000)]),
        track_stand(dir.path(), 1, "Боря", 30_000, &[(10_000, 15_000), (27_000, 29_000)]),
    ];
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = seen.clone();
    let on_progress = move |p: u8| sink.lock().unwrap().push(p);

    let out = stt::transcribe_tracks(
        &stub.client(),
        &jobs,
        &track_request(dir.path(), 30_000, 0),
        &stt_options(&ffmpeg, 90),
        Some(&on_progress),
    )
    .await
    .unwrap();

    assert!(out.complete(), "{:?}", out.failed);
    assert!(out.forced_cuts.is_empty(), "резать было нечего: {:?}", out.forced_cuts);
    assert_eq!(out.tracks.len(), 2);
    assert_eq!(*seen.lock().unwrap(), vec![50, 100], "полоса идёт по всему набору");

    // В ОБЛАКО УЕХАЛА РЕЧЬ, А НЕ ЗАПИСЬ: 17 секунд на двоих против 30 секунд
    // микса. Пер-трек здесь дешевле микса, а не дороже.
    assert_eq!(out.speech_ms(), 10_000 + 7_000);
    assert!(out.speech_ms() < 30_000, "по дорожкам уехало больше, чем весь микс");

    // Подпись берётся напрямую, без догадки.
    let merged = forge_meet::pipeline::pertrack::merge_tracks(
        &out.tracks,
        &[],
        &forge_meet::pipeline::speakers::SpeakerConfig::default(),
    );
    assert!(!merged.is_empty());
    assert!(
        merged.iter().all(|s| s.confidence == 1.0),
        "подпись дорожки — факт, а не оценка"
    );

    // И ГЛАВНОЕ: ни одного сегмента с чужим именем. Речь дорожек не
    // пересекается, поэтому каждый сегмент обязан лежать внутри интервалов
    // СВОЕГО человека.
    let own: HashMap<&str, &[(i64, i64)]> = HashMap::from([
        ("Аня", &[(0i64, 5_000i64), (20_000, 25_000)][..]),
        ("Боря", &[(10_000, 15_000), (27_000, 29_000)][..]),
    ]);
    for seg in &merged {
        let name = seg.speaker.as_deref().unwrap();
        let spans = own.get(name).unwrap_or_else(|| panic!("чужое имя {name}"));
        assert!(
            spans
                .iter()
                .any(|(s, e)| seg.start_ms >= *s && seg.end_ms <= *e),
            "сегмент {seg:?} подписан {name}, но лежит вне его речи"
        );
    }
}

/// **ОКНО КОНЧАЕТСЯ В ТИШИНЕ, А РАЗРЕЗ РЕЧИ НАЗЫВАЕТСЯ ПОИМЁННО.**
///
/// Дефект `forge-12532`: арифметическая граница окна пришлась на середину
/// реплики, и живьём это дало и пропажу 7 секунд речи, и задвоение фрагмента.
/// Здесь окна набираются из целых кусков, а единственный случай, когда кусок
/// всё-таки режется (речь без пауз длиннее окна), попадает в журнал вместе с
/// именем говорящего.
#[tokio::test]
async fn a_gap_and_a_forced_cut_both_name_the_track_they_happened_in() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = mocks::mock_ffmpeg_window_slicer(&dir.path().join("bin"), "ffmpeg").unwrap();
    let stub = start_stub().await;
    // Второе окно Ани не доедет. У Бори окно ровно одно, и оно с нулевого
    // смещения — падение адресное.
    stub.fail_window("10.000");

    let jobs = vec![
        // Аня говорит 25 секунд без единой паузы: при окне в 10 секунд её
        // речь разрезать ПРИДЁТСЯ, и это осознанный разрез.
        track_stand(dir.path(), 0, "Аня", 30_000, &[(0, 25_000)]),
        track_stand(dir.path(), 1, "Боря", 30_000, &[(26_000, 29_000)]),
    ];
    let out = stt::transcribe_tracks(
        &stub.client(),
        &jobs,
        &track_request(dir.path(), 30_000, 0),
        &stt_options(&ffmpeg, 10),
        None,
    )
    .await
    .unwrap();

    // Аня: три окна (10 + 10 + 5), Боря: одно.
    assert_eq!(out.windows, 4);
    let asked: Vec<(f64, f64)> = stub
        .calls_to("/api/v1/audio/transcriptions")
        .iter()
        .map(|c| c.window().unwrap())
        .collect();
    assert_eq!(asked, vec![(0.0, 10.0), (10.0, 10.0), (20.0, 5.0), (0.0, 3.0)]);

    // Дыра адресная: она в дорожке Ани и на общей оси встречи, а не в
    // координатах вырезанного файла.
    assert!(!out.complete());
    assert_eq!(out.failed.len(), 1);
    assert_eq!(out.failed[0].label, "Аня");
    assert_eq!(out.failed[0].track_idx, 0);
    assert_eq!(out.failed[0].axis_start_ms, 10_000);
    let note = out.gap_note().unwrap();
    assert!(note.contains("Аня"), "{note}");
    assert!(note.contains("из 4"), "{note}");

    // Разрез речи назван поимённо — иначе дыра на этом стыке выглядела бы
    // загадкой.
    assert_eq!(
        out.forced_cuts
            .iter()
            .map(|c| (c.label.as_str(), c.axis_ms))
            .collect::<Vec<_>>(),
        vec![("Аня", 10_000), ("Аня", 20_000)]
    );
    let forced = out.forced_note().unwrap();
    assert!(forced.contains("Аня"), "{forced}");

    // Речь Бори при этом доехала целиком: чужая дыра её не трогает.
    let boris = out.tracks.iter().find(|t| t.label == "Боря").unwrap();
    assert!(!boris.segments.is_empty());
    assert!(
        boris.segments.iter().all(|s| s.start_ms >= 26_000 && s.end_ms <= 29_000),
        "{:?}",
        boris.segments
    );
    // И сегменты Ани вернулись на ОБЩУЮ ось: вырезки в 30-секундной записи
    // не видно.
    let anya = out.tracks.iter().find(|t| t.label == "Аня").unwrap();
    assert!(anya.segments.iter().all(|s| s.start_ms >= 0 && s.end_ms <= 25_000));
}

/// Молчавшая всю встречу дорожка не стоит ни копейки, а дорожка без речи
/// вообще — не повод отправлять что-либо.
#[tokio::test]
async fn a_listener_track_is_never_uploaded() {
    let _g = forge_meet::testing::process_count_guard();
    let dir = tempfile::tempdir().unwrap();
    let ffmpeg = mocks::mock_ffmpeg_window_slicer(&dir.path().join("bin"), "ffmpeg").unwrap();
    let stub = start_stub().await;

    let jobs = vec![
        track_stand(dir.path(), 0, "Аня", 20_000, &[(0, 4_000)]),
        track_stand(dir.path(), 1, "Слушатель", 20_000, &[]),
    ];
    let out = stt::transcribe_tracks(
        &stub.client(),
        &jobs,
        &track_request(dir.path(), 20_000, 0),
        &stt_options(&ffmpeg, 90),
        None,
    )
    .await
    .unwrap();

    assert_eq!(out.windows, 1, "за тишину платить не за что");
    assert_eq!(stub.calls_to("/api/v1/audio/transcriptions").len(), 1);
    let listener = out.tracks.iter().find(|t| t.label == "Слушатель").unwrap();
    assert_eq!(listener.cut_ms, 0);
    assert!(listener.segments.is_empty());

    // Транзитные файлы дорожек не переживают прогон: держать на диске вторую
    // копию звука встречи незачем.
    assert!(
        !dir.path().join("work/track-0-speech.wav").exists(),
        "вырезанный файл остался на диске"
    );

    // Ни одной дорожки с речью — отправлять нечего, и это отказ этапа.
    let silent = vec![track_stand(dir.path(), 2, "Тишина", 20_000, &[])];
    let err = stt::transcribe_tracks(
        &stub.client(),
        &silent,
        &track_request(dir.path(), 20_000, 0),
        &stt_options(&ffmpeg, 90),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "no_audio");
}

// ===========================================================================
// Саммари
// ===========================================================================

fn built(text: &str) -> BuiltPrompt {
    BuiltPrompt {
        text: text.to_string(),
        truncated: false,
        used_chars: text.chars().count(),
    }
}

fn chat_options(model: &str) -> ChatOptions {
    ChatOptions {
        model: model.to_string(),
        idle_timeout: Duration::from_secs(5),
        cancel: CancellationToken::new(),
    }
}

/// Дельты доезжают до подписчика, текст собирается целиком, модель — из
/// настроек.
#[tokio::test]
async fn the_summary_streams_deltas_and_takes_its_model_from_the_options() {
    let stub = start_stub().await;
    stub.script(
        "/api/v1/chat/completions",
        Reply::Sse(chat_frames(&[
            "## Итоги\n\n",
            "Обсудили сроки. ",
            &"Длинный кусок текста. ".repeat(30),
        ])),
    );

    let seen = Arc::new(Mutex::new(Vec::<usize>::new()));
    let sink = seen.clone();
    let on_delta = move |n: usize| sink.lock().unwrap().push(n);

    let text = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>Коллеги, начинаем.</transcript>"),
        &chat_options("google/gemini-3.5-flash"),
        Some(&on_delta),
    )
    .await
    .unwrap();

    assert!(text.starts_with("## Итоги"), "{text}");
    assert!(text.contains("Обсудили сроки."), "{text}");
    assert!(
        !seen.lock().unwrap().is_empty(),
        "экран саммари не получил ни одного кадра"
    );

    let call = &stub.calls_to("/api/v1/chat/completions")[0];
    let body = call.json();
    assert_eq!(body["model"], json!("google/gemini-3.5-flash"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["messages"][0]["role"], json!("system"));
    assert_eq!(
        body["messages"][1]["content"],
        json!("<transcript>Коллеги, начинаем.</transcript>")
    );
    assert!(!call.body_text.contains(KEY), "ключ уехал в тело запроса");

    // Модель — поле настроек, а не константа.
    stub.script(
        "/api/v1/chat/completions",
        Reply::Sse(chat_frames(&["Ответ другой модели"])),
    );
    let _ = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>x</transcript>"),
        &chat_options("anthropic/claude-haiku-4.5"),
        None,
    )
    .await
    .unwrap();
    let second = &stub.calls_to("/api/v1/chat/completions")[1];
    assert_eq!(second.json()["model"], json!("anthropic/claude-haiku-4.5"));
}

/// **ОБРЫВ ПОСРЕДИ СТРИМА НЕ ТЕРЯЕТ НАПИСАННОЕ.**
#[tokio::test]
async fn a_stream_that_dies_midway_keeps_what_was_already_written() {
    let stub = start_stub().await;
    stub.script(
        "/api/v1/chat/completions",
        Reply::SseThenDrop(vec![
            format!(
                "data: {}\n\n",
                json!({"choices": [{"delta": {"content": "## Итоги\n\nПервый пункт."}}]})
            ),
            // Кадр обрывается на середине — так это и выглядит в жизни.
            "data: {\"choices\":[{\"delta\":{\"cont".to_string(),
        ]),
    );

    let out = forge_meet::cloud::summary::stream_chat(
        &stub.client(),
        "система",
        "<transcript>x</transcript>",
        &chat_options("google/gemini-3.5-flash"),
        None,
    )
    .await
    .unwrap();

    assert!(out.text.contains("Первый пункт."), "{out:?}");
    let err = out.interrupted.expect("обрыв обязан быть замечен");
    assert!(
        err.code.starts_with("cloud_"),
        "{}",
        err.full_message()
    );
    // И через фасад: неполное саммари всё равно возвращается как текст.
    stub.script(
        "/api/v1/chat/completions",
        Reply::SseThenDrop(vec![format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {"content": "Половина саммари"}}]})
        )]),
    );
    let text = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>x</transcript>"),
        &chat_options("google/gemini-3.5-flash"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(text, "Половина саммари");
}

/// Отказ до начала потока — обычная ошибка с текстом, а не пустое саммари.
#[tokio::test]
async fn an_error_status_on_the_chat_route_carries_its_body() {
    let stub = start_stub().await;
    stub.script(
        "/api/v1/chat/completions",
        Reply::Status(
            402,
            r#"{"error":{"code":"INSUFFICIENT_FUNDS","message":"Недостаточно средств"}}"#
                .to_string(),
        ),
    );
    let err = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>x</transcript>"),
        &chat_options("google/gemini-3.5-flash"),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "cloud_no_funds");
    assert!(err.full_message().contains("Недостаточно средств"), "{err}");
}

/// Молчание снимается своим таймаутом, а не висит вечно — и до первого кадра,
/// и посреди потока.
#[tokio::test]
async fn a_stream_that_goes_quiet_is_cut_by_the_idle_timeout() {
    let stub = start_stub().await;
    let mut opts = chat_options("google/gemini-3.5-flash");
    opts.idle_timeout = Duration::from_millis(300);

    // 1. Шлюз молчит с самого начала: заголовки не приезжают.
    stub.script("/api/v1/chat/completions", Reply::Hang(30));
    let started = std::time::Instant::now();
    let err = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>x</transcript>"),
        &opts,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "cloud_timeout", "{}", err.full_message());
    assert!(started.elapsed() < Duration::from_secs(10), "ждали вечно");

    // 2. Поток начался и замолчал на середине: написанное сохраняется, а
    // ожидание всё равно кончается.
    stub.script(
        "/api/v1/chat/completions",
        Reply::SseThenQuiet(
            vec![format!(
                "data: {}\n\n",
                json!({"choices": [{"delta": {"content": "Начало саммари"}}]})
            )],
            30,
        ),
    );
    let started = std::time::Instant::now();
    let out = forge_meet::cloud::summary::stream_chat(
        &stub.client(),
        "система",
        "<transcript>x</transcript>",
        &opts,
        None,
    )
    .await
    .unwrap();
    assert_eq!(out.text, "Начало саммари");
    assert_eq!(out.interrupted.unwrap().code, "cloud_timeout");
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// Отмена встречи гасит и облачное саммари.
#[tokio::test]
async fn cancelling_stops_the_summary() {
    let stub = start_stub().await;
    stub.script("/api/v1/chat/completions", Reply::Hang(30));
    let opts = chat_options("google/gemini-3.5-flash");
    let token = opts.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        token.cancel();
    });
    let err = forge_meet::cloud::summary::generate(
        &stub.client(),
        &built("<transcript>x</transcript>"),
        &opts,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "cancelled");
}

// ===========================================================================
// Роуты настроек: каталог моделей и баланс (Ф16)
// ===========================================================================

/// Состояние плагина, смотрящее на стенд вместо живого polza.ai.
fn state_on_stub(stub: &Stub, dir: &std::path::Path) -> Arc<forge_meet::state::MeetState> {
    // База отдельной переменной: инвариант герметичности узнаёт `..base`, но не
    // `..forge_meet::testing::hermetic_config(dir)` с путём к модулю.
    let base = forge_meet::testing::hermetic_config(dir);
    forge_meet::state::MeetState::new(forge_meet::config::MeetConfig {
        cloud_base_url: stub.base_url.clone(),
        polza_api_key: SecretKey::new(KEY),
        cloud_stt_model: "openai/whisper-1".to_string(),
        cloud_summary_model: "google/gemini-3.5-flash".to_string(),
        ..base
    })
}

/// **ЦЕНА ДОЕЗЖАЕТ ДО ИНТЕРФЕЙСА НЕНУЛЕВОЙ, А НЕПРИГОДНАЯ МОДЕЛЬ ПОМЕЧЕНА.**
///
/// Каталог в фикстуре — живой формы: `pricing` верхнего уровня `null` у ВСЕХ
/// записей, настоящая цена уровнем глубже. Парсер, читающий верхний уровень,
/// покраснеет здесь, а не через месяц в счёте.
///
/// Вторая половина теста про `openai/whisper-large-v3-turbo`: он втрое дешевле
/// и не даёт таймкодов ВООБЩЕ, то есть его выбор ломает атрибуцию спикеров
/// целиком. Флаг приезжает С СЕРВЕРА — знание о пригодности модели живёт в
/// клиенте сервиса, а не в третьей копии во фронте.
#[tokio::test]
async fn the_models_route_carries_prices_and_marks_the_model_without_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let stub = start_stub().await;
    stub.script(
        "/api/v1/models",
        Reply::Json(json!({"object": "list", "data": [
            {"id": "openai/whisper-1", "name": "Whisper 1", "type": "stt", "pricing": null,
             "top_provider": {"context_length": 0,
                 "pricing": {"stt_per_minute": "0.43200000", "currency": "RUB"}}},
            {"id": "openai/whisper-large-v3-turbo", "type": "stt", "pricing": null,
             "top_provider": {"context_length": 0,
                 "pricing": {"stt_per_minute": "0.04800000", "currency": "RUB"}}},
            {"id": "google/gemini-3.5-flash", "name": "Gemini 3.5 Flash", "type": "chat",
             "pricing": null,
             "top_provider": {"context_length": 1048576, "max_completion_tokens": 65536,
                 "pricing": {"prompt_per_million": "176.05000000",
                             "completion_per_million": "1056.32000000", "currency": "RUB"}}},
            {"id": "openai/tts-1", "type": "tts", "pricing": null, "top_provider": {}}
        ]})),
    );

    let state = state_on_stub(&stub, dir.path());
    let axum::Json(dto) = forge_meet::routes::cloud::models(State(state)).await;

    assert!(dto.configured);
    assert!(dto.error.is_none(), "{:?}", dto.error);
    assert_eq!(dto.total, 4, "счётчик считает ВЕСЬ каталог, а не выборку");
    assert_eq!(dto.chat.len(), 1, "tts/image/video в списки стадий не попадают");
    assert_eq!(dto.stt.len(), 2);

    let chat = &dto.chat[0];
    let p = chat
        .pricing
        .as_ref()
        .expect("цена не прочиталась: парсер смотрит на верхний уровень, где у всех null");
    assert_eq!(p.prompt_per_million, Some(176.05));
    assert_eq!(p.completion_per_million, Some(1056.32));
    assert_eq!(chat.context_length, Some(1_048_576));
    assert!(chat.selected, "выбранная в конфиге модель помечена");
    assert_eq!(chat.timestamps, None, "у чата вопроса о таймкодах нет");

    let good = dto.stt.iter().find(|m| m.id == "openai/whisper-1").unwrap();
    assert_eq!(good.timestamps, Some(true));
    assert!(good.selected);
    assert_eq!(good.pricing.as_ref().unwrap().stt_per_minute, Some(0.432));

    let bad = dto
        .stt
        .iter()
        .find(|m| m.id == "openai/whisper-large-v3-turbo")
        .unwrap();
    assert_eq!(
        bad.timestamps,
        Some(false),
        "модель без таймкодов обязана быть помечена, а не выглядеть просто дешевле"
    );
    assert!(!bad.selected);
    assert_eq!(bad.pricing.as_ref().unwrap().stt_per_minute, Some(0.048));

    // Ключ ушёл ТОЛЬКО заголовком и только на сервере: наружу уехали цифры.
    let calls = stub.calls_to("/api/v1/models");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].auth.as_deref(), Some(&format!("Bearer {KEY}")[..]));
    let body = serde_json::to_string(&dto).unwrap();
    assert!(!body.contains(KEY), "ключ утёк в ответ роута");
}

/// Баланс приходит в РУБЛЯХ и вместе с лимитом ключа.
#[tokio::test]
async fn the_balance_route_shows_roubles_and_the_key_limit() {
    let dir = tempfile::tempdir().unwrap();
    let stub = start_stub().await;
    let state = state_on_stub(&stub, dir.path());

    let axum::Json(dto) = forge_meet::routes::cloud::balance(State(state)).await;
    assert!(dto.configured);
    assert!(dto.error.is_none(), "{:?}", dto.error);
    let b = dto.balance.clone().expect("баланс не приехал");
    assert!((b.amount - 461.88706171).abs() < 1e-8, "{b:?}");
    assert_eq!(b.currency, "RUB");
    let key = dto.key.clone().expect("лимиты ключа не приехали");
    assert_eq!(key.limit_remaining, Some(288.7275241));
    assert_eq!(key.limit_reset.as_deref(), Some("monthly"));

    // Лимиты берутся из КЭШИРОВАННОЙ пробы, а баланс — свежим вызовом.
    assert_eq!(stub.calls_to("/api/v1/balance").len(), 1);
    assert_eq!(stub.calls_to("/api/v1/key").len(), 1);
    assert!(!serde_json::to_string(&dto).unwrap().contains(KEY));
}
