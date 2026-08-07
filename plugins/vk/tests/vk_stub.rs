//! Локальный стенд, изображающий `api.vk.com`, LongPoll-сервер и
//! upload-сервер — флагманский тест крейта.
//!
//! Секретов не требует, работает без сети наружу и гоняет ВЕСЬ транспорт через
//! настоящий `reqwest`: разбор конверта, троттлер, политику ретраев,
//! идемпотентность `random_id`, потоковую загрузку и скачивание.
//!
//! # Почему свой стенд, а не `wiremock`/`httpmock`
//!
//! Новых dev-зависимостей крейт не заводит намеренно: стенд собирается из уже
//! имеющихся `axum` + `tokio::net::TcpListener::bind("127.0.0.1:0")` и
//! занимает меньше места, чем конфигурация мок-сервера. Ровно ради этого
//! `VkConfig::base_url` объявлен обязательным ещё в Ф1.
//!
//! # Как стенд роняет соединение
//!
//! «Транспортный отказ» изображается ответом, у которого объявлен
//! `Content-Length`, а тело обрывается ошибкой на первом же кадре: заголовки
//! доезжают, тело — нет. Для клиента это неотличимо от обрыва соединения в
//! середине ответа, то есть ровно тот случай, ради которого существует
//! однократный повтор.
//!
//! # Порт
//!
//! Всегда `127.0.0.1:0` — операционная система выдаёт свободный. Жёсткий порт
//! в тесте однажды упёрся бы в живой инстанс devforge на 7331.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde_json::{json, Value};

use forge_vk::api::error::{to_api_error, VkError};
use forge_vk::api::methods::{Captcha, SendBody};
use forge_vk::api::VkClient;
use forge_vk::config::{SecretToken, VkConfig};
use forge_vk::state::ConnHealth;

/// Токен, который стенд обязан увидеть в ТЕЛЕ запроса и который не имеет
/// права появиться нигде больше.
const TOKEN: &str = "vk1.a.STUB_ACCESS_TOKEN_9876543210";

// ===========================================================================
// Стенд.
// ===========================================================================

/// Что стенд ответит на очередной вызов метода.
#[derive(Debug, Clone)]
enum Reply {
    /// Успех: `{"response": …}`.
    Response(Value),
    /// Отказ VK — с HTTP **200**, как в жизни.
    Error(Value),
    /// Оборвать тело ответа (см. шапку модуля).
    Drop,
    /// Голый HTTP-статус (прокси, балансировщик, 5xx самого VK).
    Status(u16),
    /// Не-JSON (captive portal).
    Garbage,
}

/// Запись о вызове.
#[derive(Debug, Clone)]
struct Call {
    method: String,
    params: BTreeMap<String, String>,
    query: String,
    at: Instant,
}

/// Состояние стенда.
#[derive(Default)]
struct StubState {
    /// Метод → очередь заготовленных ответов. Пусто ⇒ дефолтный успех.
    script: Mutex<HashMap<String, VecDeque<Reply>>>,
    /// Журнал вызовов методов API.
    calls: Mutex<Vec<Call>>,
    /// Очередь ответов LongPoll-сервера.
    lp_script: Mutex<VecDeque<Value>>,
    /// Журнал обращений к LongPoll.
    lp_calls: Mutex<Vec<LpCall>>,
    /// Тела, принятые upload-сервером.
    uploads: Mutex<Vec<Upload>>,
    /// Журнал обращений к «CDN» вложений.
    cdn_calls: Mutex<Vec<String>>,
    /// Очередь статусов «CDN»: `None`/пусто ⇒ отдать байты.
    cdn_script: Mutex<VecDeque<u16>>,
}

/// Обращение к LongPoll-серверу.
///
/// Query записывается ЦЕЛИКОМ и это принципиально: инвариант «после `failed:2`
/// воркер НЕ сбросил `ts`» проверяется именно тем, с каким `ts` пришёл
/// следующий `a_check`, — по кадрам его не увидеть.
#[derive(Debug, Clone)]
struct LpCall {
    /// Когда пришёл запрос.
    at: Instant,
    /// Параметры `act`/`key`/`ts`/`wait`/`mode`/`version`.
    params: BTreeMap<String, String>,
}

/// Что upload-сервер увидел на входе.
#[derive(Debug, Clone)]
struct Upload {
    /// Объявленный `Content-Length` — без него `reqwest` ушёл бы в chunked.
    declared_len: Option<u64>,
    /// Фактически прочитано байт (тело + границы multipart).
    actual_len: usize,
    /// Сырое тело — по нему видно имя поля, имя файла и содержимое.
    body: Vec<u8>,
}

impl StubState {
    fn script(&self, method: &str, replies: Vec<Reply>) {
        self.script
            .lock()
            .unwrap()
            .insert(method.to_string(), replies.into());
    }

    fn lp_script(&self, replies: Vec<Value>) {
        *self.lp_script.lock().unwrap() = replies.into();
    }

    /// Что «CDN» ответит на очередные запросы (статус != 200 ⇒ отказ).
    fn cdn_script(&self, statuses: Vec<u16>) {
        *self.cdn_script.lock().unwrap() = statuses.into();
    }

    fn cdn_calls(&self) -> Vec<String> {
        self.cdn_calls.lock().unwrap().clone()
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == method)
            .collect()
    }

    fn lp_calls(&self) -> Vec<LpCall> {
        self.lp_calls.lock().unwrap().clone()
    }

    /// Значения параметра по всем обращениям к LongPoll (`ts`, `key`, `mode`).
    fn lp_param(&self, key: &str) -> Vec<String> {
        self.lp_calls()
            .into_iter()
            .map(|c| c.params.get(key).cloned().unwrap_or_default())
            .collect()
    }
}

/// Адреса поднятого стенда.
struct Stub {
    /// Корень стенда (для заведомо несуществующих путей).
    root: String,
    /// Значение для `VkConfig::base_url`.
    base_url: String,
    /// Значение, которое стенд «выдаёт» как LongPoll-сервер (СО СХЕМОЙ —
    /// живой VK отдаёт голый хост, а локальный стенд не умеет TLS).
    lp_server: String,
    /// Адрес upload-сервера.
    upload_url: String,
    /// Базовый адрес «CDN» вложений.
    cdn_url: String,
    state: Arc<StubState>,
}

/// Поднимает стенд на свободном порту.
async fn start_stub() -> Stub {
    install_log_capture();
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .route("/method/:name", post(handle_method))
        .route("/lp", get(handle_longpoll))
        .route("/upload/:kind", post(handle_upload))
        .route("/cdn/:name", get(handle_cdn))
        .route("/captcha.php", get(handle_captcha_image))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Stub {
        root: format!("http://{addr}"),
        base_url: format!("http://{addr}/method"),
        lp_server: format!("http://{addr}/lp"),
        upload_url: format!("http://{addr}/upload/photo"),
        cdn_url: format!("http://{addr}/cdn"),
        state,
    }
}

/// Ответ с оборванным телом — «соединение упало в середине».
fn broken_response() -> Response {
    let stream = futures_util::stream::once(async {
        Err::<Vec<u8>, std::io::Error>(std::io::Error::other("stub: connection dropped"))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        // Заголовки доезжают, тело — нет.
        .header(axum::http::header::CONTENT_LENGTH, "4096")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn handle_method(
    State(state): State<Arc<StubState>>,
    Path(name): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    Form(params): Form<BTreeMap<String, String>>,
) -> Response {
    state.calls.lock().unwrap().push(Call {
        method: name.clone(),
        params,
        query: query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
        at: Instant::now(),
    });

    let reply = state
        .script
        .lock()
        .unwrap()
        .get_mut(&name)
        .and_then(VecDeque::pop_front);

    match reply {
        Some(Reply::Response(v)) => axum::Json(json!({ "response": v })).into_response(),
        Some(Reply::Error(v)) => {
            // ВАЖНО: ошибка VK приезжает с HTTP 200.
            (StatusCode::OK, axum::Json(json!({ "error": v }))).into_response()
        }
        Some(Reply::Drop) => broken_response(),
        Some(Reply::Status(code)) => (
            StatusCode::from_u16(code).unwrap(),
            "upstream is having a bad day",
        )
            .into_response(),
        Some(Reply::Garbage) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            "<html>captive portal</html>",
        )
            .into_response(),
        // Дефолт: скучный успех. Тесты, которым важен только факт вызова,
        // не обязаны ничего скриптовать.
        None => axum::Json(json!({ "response": 1 })).into_response(),
    }
}

async fn handle_longpoll(
    State(state): State<Arc<StubState>>,
    Query(params): Query<BTreeMap<String, String>>,
) -> Response {
    state.lp_calls.lock().unwrap().push(LpCall {
        at: Instant::now(),
        params,
    });
    let reply = state.lp_script.lock().unwrap().pop_front();
    if let Some(v) = reply {
        return axum::Json(v).into_response();
    }

    // Скрипт кончился — ведём себя как настоящий LongPoll-сервер: держим
    // соединение `wait` секунд и отвечаем пустой пачкой с ТЕМ ЖЕ `ts`.
    //
    // И удержание, и сохранение `ts` обязательны. Мгновенный ответ превратил
    // бы «сценарий доиграл» в busy-loop из тысяч запросов в секунду (тест
    // грелся бы вместо того, чтобы проверять), а `ts: 1` откатил бы курсор
    // назад и сломал ровно тот инвариант, ради которого стенд и написан.
    let (ts, wait) = {
        let calls = state.lp_calls.lock().unwrap();
        let last = calls.last();
        (
            last.and_then(|c| c.params.get("ts").cloned())
                .unwrap_or_else(|| "1".to_string()),
            last.and_then(|c| c.params.get("wait").and_then(|w| w.parse::<u64>().ok()))
                .unwrap_or(1)
                .min(2),
        )
    };
    tokio::time::sleep(Duration::from_secs(wait)).await;
    axum::Json(json!({"ts": ts, "updates": []})).into_response()
}

async fn handle_upload(
    State(state): State<Arc<StubState>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    state.uploads.lock().unwrap().push(Upload {
        declared_len: declared,
        actual_len: body.len(),
        body: body.to_vec(),
    });

    // Форма ответа upload-сервера VK: голый объект БЕЗ конверта `response`.
    let payload = if kind == "photo" {
        json!({"server": 123456, "photo": "[{\"photo\":\"x\"}]", "hash": "deadbeef"})
    } else {
        json!({"file": "doc123456_789|abcdef"})
    };
    axum::Json(payload).into_response()
}

async fn handle_cdn(State(state): State<Arc<StubState>>, Path(name): Path<String>) -> Response {
    state.cdn_calls.lock().unwrap().push(name.clone());
    // Протухшую подписанную ссылку VK изображает статусом из скрипта: именно
    // на 403/404 роут медиа обязан сделать РОВНО ОДИН повтор с новым адресом.
    if let Some(status) = state.cdn_script.lock().unwrap().pop_front() {
        if status != 200 {
            return (
                StatusCode::from_u16(status).unwrap(),
                "the link has expired",
            )
                .into_response();
        }
    }
    // Байты, а не JSON: это CDN, и клиент обязан уметь читать их потоком.
    // Имя вида `<size>` или `<size>-<что-угодно>` — размер берётся из головы,
    // хвост различает адреса разных вложений одного сообщения.
    let size: usize = name
        .split('-')
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
        vec![b'v'; size],
    )
        .into_response()
}

/// Картинка captcha — единственный ответ стенда с `Content-Type: image/*`.
///
/// Прокси `GET /api/vk/captcha/:sid` обязан отдать её байты и НЕ отдать
/// что-либо, картинкой не являющееся (страница-заглушка вместо картинки —
/// это пустая рамка без объяснений в UI).
async fn handle_captcha_image() -> Response {
    // Минимальный валидный PNG-заголовок: содержимое нам безразлично, важна
    // только неизменность байтов на всём пути.
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        png,
    )
        .into_response()
}

// ===========================================================================
// Захват логов.
// ===========================================================================

/// Минимальный `tracing::Subscriber`, складывающий события в строки.
///
/// Написан руками, чтобы не заводить dev-зависимость `tracing-subscriber`
/// ради одного инварианта («токена нет в логах»).
struct LogCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

/// Общий буфер логов процесса.
static LOG_LINES: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> = std::sync::OnceLock::new();

/// Ставит захват логов ГЛОБАЛЬНО и ровно один раз на процесс.
///
/// Именно глобально, а не `set_default` на поток: `tracing` кэширует «интерес»
/// к callsite ОДИН раз на весь процесс, и тест, выполнившийся первым без
/// подписчика, навсегда пометил бы `tracing::warn!` в `api/mod.rs` как
/// неинтересный — тогда захват в соседнем тесте молча получал бы пустоту.
/// Вызывается из [`start_stub`], то есть заведомо раньше первого события.
fn install_log_capture() -> Arc<Mutex<Vec<String>>> {
    LOG_LINES
        .get_or_init(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let _ = tracing::subscriber::set_global_default(LogCapture {
                lines: lines.clone(),
            });
            lines
        })
        .clone()
}

struct FieldCollector(String);

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push_str(&format!("{}={:?} ", field.name(), value));
    }
}

impl tracing::Subscriber for LogCapture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut collector = FieldCollector(String::new());
        event.record(&mut collector);
        self.lines.lock().unwrap().push(collector.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

// ===========================================================================
// Сборка клиента.
// ===========================================================================

fn config(stub: &Stub, rps: u32) -> VkConfig {
    VkConfig {
        access_token: SecretToken::new(TOKEN),
        base_url: stub.base_url.clone(),
        rate_limit_rps: rps,
        cache_dir: std::env::temp_dir().join("forge-vk-stub-cache"),
        ..VkConfig::default()
    }
}

/// Клиент с КОРОТКИМИ паузами повтора: политику проверяем, а не выдержку.
fn client(stub: &Stub, rps: u32) -> VkClient {
    VkClient::new(
        &config(stub, rps),
        Arc::new(tokio::sync::RwLock::new(ConnHealth::default())),
    )
    .unwrap()
    .with_retry_delays(Duration::from_millis(10), Duration::from_millis(20))
}

fn vk_error(code: i64, msg: &str) -> Value {
    json!({
        "error_code": code,
        "error_msg": msg,
        // Ровно то, чем VK возвращает наш собственный токен обратно.
        "request_params": [
            {"key": "method", "value": "messages.getConversations"},
            {"key": "access_token", "value": TOKEN},
            {"key": "v", "value": "5.199"}
        ]
    })
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// Базовая проверка: успешный вызов доезжает, токен уходит в ТЕЛЕ (а не в
/// query), версия API подставляется.
#[tokio::test]
async fn successful_call_sends_the_token_in_the_body() {
    let stub = start_stub().await;
    stub.state
        .script("users.get", vec![Reply::Response(json!([{"id": 1}]))]);

    let users = client(&stub, 3).users_get(&[1]).await.unwrap();
    assert_eq!(users.len(), 1);

    let calls = stub.state.calls_of("users.get");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].params.get("access_token").unwrap(), TOKEN);
    assert_eq!(calls[0].params.get("v").unwrap(), "5.199");
    assert_eq!(calls[0].params.get("user_ids").unwrap(), "1");
    assert!(
        calls[0].query.is_empty(),
        "токен обязан ехать в теле, а не в query: {}",
        calls[0].query
    );
}

/// ИНВАРИАНТ ФАЗЫ сквозь настоящий HTTP: `request_params` не доезжают ни до
/// ошибки, ни до тела ответа, ни до логов.
#[tokio::test]
async fn error_request_params_are_stripped_over_real_http() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Error(vk_error(5, "User authorization failed"))],
    );

    let lines = install_log_capture();
    let from = lines.lock().unwrap().len();

    let err = client(&stub, 3)
        .get_conversations(20, 0, "all")
        .await
        .unwrap_err();

    assert_eq!(err.code(), Some(5));
    assert!(!format!("{err:?}").contains(TOKEN), "{err:?}");
    assert!(!format!("{err:?}").contains("STUB_ACCESS_TOKEN"), "{err:?}");
    assert!(!format!("{err:?}").contains("request_params"), "{err:?}");

    let api = to_api_error(&err);
    assert_eq!(api.status, StatusCode::CONFLICT);
    assert_eq!(api.body.code, "not_authorized");
    let rendered = serde_json::to_string(&api.body).unwrap();
    assert!(!rendered.contains("STUB_ACCESS_TOKEN"), "{rendered}");

    // Проверяем ВЕСЬ буфер процесса, а не только свои строки: если токен
    // просочился в лог любого другого теста этого файла, ловится тоже здесь.
    let all = lines.lock().unwrap().clone();
    let logs = all.join("\n");
    assert!(!logs.is_empty(), "клиент обязан что-то писать в лог");
    assert!(!logs.contains(TOKEN), "токен в логах:\n{logs}");
    assert!(!logs.contains("STUB_ACCESS_TOKEN"), "токен в логах:\n{logs}");
    let mine = all[from..].join("\n");
    assert!(
        mine.contains("messages.getConversations"),
        "метод в логах быть обязан:\n{mine}"
    );
}

/// Коды 5 / 14 / 15 — три перехода автомата логина (Ф3). Проверяем маппинг
/// сквозь настоящий HTTP, включая поля captcha.
#[tokio::test]
async fn codes_five_fourteen_fifteen_map_to_the_contract() {
    let stub = start_stub().await;
    let c = client(&stub, 20);

    // 5 — токен невалиден.
    stub.state
        .script("users.get", vec![Reply::Error(vk_error(5, "auth failed"))]);
    let api = to_api_error(&c.users_get(&[1]).await.unwrap_err());
    assert_eq!(api.status, StatusCode::CONFLICT);
    assert_eq!(api.body.code, "not_authorized");

    // 14 — captcha; sid и картинка обязаны доехать до фронта.
    stub.state.script(
        "users.get",
        vec![Reply::Error(json!({
            "error_code": 14,
            "error_msg": "Captcha needed",
            "captcha_sid": "239123456",
            "captcha_img": "https://api.vk.com/captcha.php?sid=239123456",
            "request_params": [{"key": "access_token", "value": TOKEN}]
        }))],
    );
    let api = to_api_error(&c.users_get(&[1]).await.unwrap_err());
    assert_eq!(api.body.code, "captcha_required");
    assert_eq!(api.body.captcha_sid.as_deref(), Some("239123456"));
    assert!(api.body.captcha_img.as_deref().unwrap().contains("captcha.php"));

    // 15 — у токена нет права `messages`; отдельный экран, а не «ошибка».
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Error(vk_error(15, "Access denied"))],
    );
    let api = to_api_error(&c.get_conversations(20, 0, "all").await.unwrap_err());
    assert_eq!(api.body.code, "no_messages_scope");
    assert_eq!(api.status, StatusCode::CONFLICT);
}

/// ГЛАВНЫЙ тест идемпотентности: стенд роняет первый `POST messages.send`,
/// принимает второй — `random_id` обязан СОВПАСТЬ, иначе собеседник получил
/// бы два сообщения.
#[tokio::test]
async fn transport_retry_reuses_random_id() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.send",
        vec![Reply::Drop, Reply::Response(json!(424242))],
    );

    let random_id = forge_vk::api::methods::new_random_id();
    let sent = client(&stub, 20)
        .send_message(
            1234,
            random_id,
            SendBody {
                message: Some("привет".into()),
                ..SendBody::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(sent, 424242, "повтор обязан вернуть id из второго ответа");

    let calls = stub.state.calls_of("messages.send");
    assert_eq!(calls.len(), 2, "ровно один повтор");
    assert_eq!(
        calls[0].params.get("random_id"),
        calls[1].params.get("random_id"),
        "random_id при повторе обязан совпадать"
    );
    assert_eq!(
        calls[0].params.get("random_id").unwrap(),
        &random_id.to_string()
    );
    // И весь остальной запрос тоже тот же — иначе «повтор» был бы новым
    // сообщением.
    assert_eq!(calls[0].params, calls[1].params);
}

/// Транспортный отказ повторяется РОВНО один раз: второй отказ — это уже
/// состояние, а не рябь.
#[tokio::test]
async fn transport_failure_is_retried_exactly_once() {
    let stub = start_stub().await;
    stub.state
        .script("users.get", vec![Reply::Drop, Reply::Drop, Reply::Drop]);

    let err = client(&stub, 20).users_get(&[1]).await.unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(
        stub.state.calls_of("users.get").len(),
        2,
        "одна попытка + один повтор"
    );
    assert_eq!(to_api_error(&err).status, StatusCode::SERVICE_UNAVAILABLE);
}

/// HTTP 5xx лечится тем же одним повтором, 4xx — нет.
#[tokio::test]
async fn http_5xx_is_retried_and_4xx_is_not() {
    let stub = start_stub().await;

    stub.state.script(
        "users.get",
        vec![Reply::Status(503), Reply::Response(json!([{"id": 1}]))],
    );
    assert_eq!(client(&stub, 20).users_get(&[1]).await.unwrap().len(), 1);
    assert_eq!(stub.state.calls_of("users.get").len(), 2);

    stub.state.calls.lock().unwrap().clear();
    stub.state.script("users.get", vec![Reply::Status(404)]);
    let err = client(&stub, 20).users_get(&[1]).await.unwrap_err();
    assert_eq!(err, VkError::Http { status: 404 });
    assert_eq!(
        stub.state.calls_of("users.get").len(),
        1,
        "4xx — не рябь, прокси так и будет отвечать"
    );
}

/// Код 6 (промах нашего троттлера) чинится молча одним повтором; код 9
/// (flood control) не повторяется НИКОГДА — автоповтор продлевает бан.
#[tokio::test]
async fn code_six_is_retried_once_and_code_nine_is_never() {
    let stub = start_stub().await;

    stub.state.script(
        "messages.markAsRead",
        vec![
            Reply::Error(vk_error(6, "Too many requests per second")),
            Reply::Response(json!(1)),
        ],
    );
    client(&stub, 20)
        .mark_as_read(1234, None, None)
        .await
        .expect("код 6 обязан чиниться повтором, не доходя до пользователя");
    assert_eq!(stub.state.calls_of("messages.markAsRead").len(), 2);

    stub.state.script(
        "messages.setActivity",
        vec![
            Reply::Error(vk_error(9, "Flood control")),
            Reply::Response(json!(1)),
        ],
    );
    let err = client(&stub, 20)
        .set_activity(1234, "typing")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(9));
    assert_eq!(
        stub.state.calls_of("messages.setActivity").len(),
        1,
        "код 9 не повторяется"
    );
    let api = to_api_error(&err);
    assert_eq!(api.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(api.body.retry_after_secs, Some(60));

    // Две шестёрки подряд (forge-zzy7) — это ещё в пределах бюджета
    // MAX_CODE6_RETRIES=2: стартовый залп (проверка токена + курсор
    // LongPoll + список бесед) регулярно даёт именно такую картину, и один
    // повтор её не разгребает. Пользователь не должен увидеть ошибку.
    stub.state.script(
        "messages.markAsRead",
        vec![
            Reply::Error(vk_error(6, "Too many")),
            Reply::Error(vk_error(6, "Too many")),
            Reply::Response(json!(1)),
        ],
    );
    stub.state.calls.lock().unwrap().clear();
    client(&stub, 20)
        .mark_as_read(1234, None, None)
        .await
        .expect("две шестёрки подряд обязаны чиниться вторым повтором");
    assert_eq!(stub.state.calls_of("messages.markAsRead").len(), 3);

    // Три шестёрки подряд — бюджет исчерпан, честный 429 пользователю.
    stub.state.script(
        "messages.markAsRead",
        vec![
            Reply::Error(vk_error(6, "Too many")),
            Reply::Error(vk_error(6, "Too many")),
            Reply::Error(vk_error(6, "Too many")),
        ],
    );
    stub.state.calls.lock().unwrap().clear();
    let err = client(&stub, 20)
        .mark_as_read(1234, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(6));
    assert_eq!(stub.state.calls_of("messages.markAsRead").len(), 3);
}

/// Не-JSON от captive portal превращается в `decode`, а не в панику.
#[tokio::test]
async fn garbage_body_becomes_a_decode_error() {
    let stub = start_stub().await;
    stub.state.script("users.get", vec![Reply::Garbage]);
    let err = client(&stub, 20).users_get(&[1]).await.unwrap_err();
    assert!(matches!(err, VkError::Decode { .. }), "{err:?}");
    assert_eq!(
        stub.state.calls_of("users.get").len(),
        1,
        "нечитаемый ответ не повторяется"
    );
}

/// Троттлер на настоящем HTTP.
///
/// Проверяются ДВА свойства, которых нет у фиксированного окна:
/// 1. стартовый залп ёмкостью бака проходит мгновенно;
/// 2. дальше вызовы идут РАВНОМЕРНО (~1/rps), а не «три сразу, потом секунда
///    тишины», и в любую скользящую секунду установившегося режима их не
///    больше трёх.
#[tokio::test]
async fn throttle_spaces_calls_evenly() {
    let stub = start_stub().await;
    let c = client(&stub, 3);

    for _ in 0..7 {
        c.mark_as_read(1234, None, None).await.unwrap();
    }

    let calls = stub.state.calls_of("messages.markAsRead");
    assert_eq!(calls.len(), 7);
    let at: Vec<Instant> = calls.iter().map(|c| c.at).collect();

    // 1. Залп из трёх — мгновенно.
    let burst = at[2].duration_since(at[0]);
    assert!(
        burst < Duration::from_millis(200),
        "залп ёмкостью бака обязан проходить сразу, занял {burst:?}"
    );

    // 2. Дальше — равномерно. Фиксированное окно дало бы здесь нули и
    //    секундные провалы.
    for i in 3..7 {
        let gap = at[i].duration_since(at[i - 1]);
        assert!(
            gap >= Duration::from_millis(280) && gap <= Duration::from_millis(800),
            "интервал {i} = {gap:?} — троттлер обязан разносить вызовы ровно"
        );
    }

    // 3. Скользящая секунда установившегося режима: не больше трёх.
    let window = at[6].duration_since(at[3]);
    assert!(
        window >= Duration::from_millis(950),
        "четыре подряд уложились в {window:?} — это больше 3 запросов в секунду"
    );
}

/// LongPoll НЕ троттлится: при `rate_limit_rps = 1` три обычных вызова заняли
/// бы две секунды, а три `a_check` обязаны пройти мгновенно.
#[tokio::test]
async fn longpoll_check_bypasses_the_throttle() {
    let stub = start_stub().await;
    stub.state.lp_script(vec![
        json!({"ts": 2, "updates": [[4, 7010, 17, 1234]]}),
        json!({"failed": 1, "ts": 3}),
        json!({"failed": 2}),
    ]);

    let c = client(&stub, 1);
    let started = Instant::now();
    let mode = forge_vk::longpoll::LP_MODE;
    let first = c
        .longpoll_check(&stub.lp_server, "KEY", 1, 1, mode)
        .await
        .unwrap();
    let second = c
        .longpoll_check(&stub.lp_server, "KEY", 2, 1, mode)
        .await
        .unwrap();
    let third = c
        .longpoll_check(&stub.lp_server, "KEY", 3, 1, mode)
        .await
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(first["ts"], 2);
    assert_eq!(second["failed"], 1);
    assert_eq!(third["failed"], 2);
    assert!(
        elapsed < Duration::from_millis(500),
        "a_check обязан обходить троттлер, заняло {elapsed:?}"
    );
    assert_eq!(stub.state.lp_calls().len(), 3);
    // Бит 32 (`pts`) обязан доехать до сервера: без него `a_check` не отдаёт
    // `pts`, и `messages.getLongPollHistory` вызвать нечем.
    assert_eq!(stub.state.lp_param("mode"), vec!["234", "234", "234"]);
    assert_eq!(stub.state.lp_param("version"), vec!["3", "3", "3"]);

    // …и бюджет API при этом не израсходован.
    assert_eq!(c.throttle_stats().await.total_waits, 0);
}

/// Загрузка идёт ПОТОКОМ и с объявленной длиной: без `Content-Length`
/// `reqwest` перешёл бы на chunked, который upload-сервер VK не принимает.
#[tokio::test]
async fn upload_multipart_streams_with_a_declared_length() {
    let stub = start_stub().await;
    let payload = vec![b'A'; 4096];
    let size = payload.len() as u64;
    let chunks: Vec<std::io::Result<bytes::Bytes>> = payload
        .chunks(1024)
        .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
        .collect();
    let stream = futures_util::stream::iter(chunks);

    let c = client(&stub, 1);
    let saved = c
        .upload_multipart(&stub.upload_url, "photo", "снимок.jpg", size, stream)
        .await
        .unwrap();
    assert_eq!(saved["server"], 123456);
    assert_eq!(saved["hash"], "deadbeef");

    {
        let uploads = stub.state.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        let got = &uploads[0];
        assert!(
            got.declared_len.is_some(),
            "Content-Length обязан быть объявлен"
        );
        assert!(got.actual_len > 4096, "тело + границы multipart");
        let text = String::from_utf8_lossy(&got.body);
        assert!(text.contains("name=\"photo\""), "имя поля");
        assert!(text.contains(".jpg"), "имя файла обязано доехать");
        assert!(text.contains("AAAA"), "содержимое обязано доехать");
    }

    // Загрузка идёт мимо троттлера: бюджет API не тронут.
    assert_eq!(c.throttle_stats().await.total_waits, 0);
}

/// Скачивание вложения — поток байтов, тоже мимо троттлера.
#[tokio::test]
async fn get_stream_downloads_bytes_without_throttling() {
    let stub = start_stub().await;
    let c = client(&stub, 1);

    let started = Instant::now();
    let mut total = 0usize;
    for _ in 0..3 {
        let resp = c.get_stream(&format!("{}/65536", stub.cdn_url)).await.unwrap();
        total += resp.bytes().await.unwrap().len();
    }
    assert_eq!(total, 3 * 65536);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "CDN обязан обходить троттлер"
    );
    assert_eq!(c.throttle_stats().await.total_waits, 0);

    // Протухшая подписанная ссылка VK отвечает статусом ошибки — это
    // `VkError::Http`, а не паника и не пустой поток.
    let err = c
        .get_stream(&format!("{}/nope", stub.root))
        .await
        .unwrap_err();
    assert_eq!(err, VkError::Http { status: 404 });
}

/// Здоровье соединения наполняется реальными вызовами: транспортный отказ
/// ставит `Down`, следующий успех — `Ok`. Без этого `503 not_connected` в
/// data-роутах опирался бы на пустоту.
#[tokio::test]
async fn connection_health_follows_real_calls() {
    let stub = start_stub().await;
    let health = Arc::new(tokio::sync::RwLock::new(ConnHealth::default()));
    let c = VkClient::new(&config(&stub, 20), health.clone())
        .unwrap()
        .with_retry_delays(Duration::from_millis(10), Duration::from_millis(10));

    stub.state
        .script("users.get", vec![Reply::Drop, Reply::Drop]);
    assert!(c.users_get(&[1]).await.is_err());
    assert!(
        matches!(&*health.read().await, ConnHealth::Down { .. }),
        "транспортный отказ обязан отметиться"
    );

    stub.state
        .script("users.get", vec![Reply::Response(json!([{"id": 1}]))]);
    c.users_get(&[1]).await.unwrap();
    assert!(matches!(&*health.read().await, ConnHealth::Ok { .. }));

    // Отказ VK по существу (код 100 по живой сети) здоровье НЕ роняет.
    stub.state
        .script("users.get", vec![Reply::Error(vk_error(100, "bad param"))]);
    assert!(c.users_get(&[1]).await.is_err());
    assert!(
        matches!(&*health.read().await, ConnHealth::Ok { .. }),
        "код 100 приехал по живой сети — связь исправна"
    );
}

/// Captcha-повтор с фронта = ТОТ ЖЕ запрос плюс два поля.
#[tokio::test]
async fn captcha_answer_is_forwarded_verbatim() {
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(777))]);

    let random_id = forge_vk::api::methods::new_random_id();
    client(&stub, 20)
        .send_message(
            1234,
            random_id,
            SendBody {
                message: Some("текст".into()),
                captcha: Some(Captcha {
                    sid: "239123456".into(),
                    key: "abcde".into(),
                }),
                ..SendBody::default()
            },
        )
        .await
        .unwrap();

    let calls = stub.state.calls_of("messages.send");
    assert_eq!(calls[0].params.get("captcha_sid").unwrap(), "239123456");
    assert_eq!(calls[0].params.get("captcha_key").unwrap(), "abcde");
    assert_eq!(calls[0].params.get("message").unwrap(), "текст");
}

// ===========================================================================
// Ф3 — логин и captcha сквозь настоящий HTTP.
// ===========================================================================

/// Состояние плагина, смотрящее на стенд, с ИЗОЛИРОВАННЫМ `vk.toml`.
///
/// Изоляция обязательна: `POST /api/vk/login/token` пишет токен на диск, а
/// `POST /api/vk/logout` его стирает — на дефолтных путях прогон `cargo test`
/// разлогинивал бы пользователя.
fn login_state(stub: &Stub, dir: &tempfile::TempDir, token: &str) -> Arc<forge_vk::VkState> {
    forge_vk::VkState::new(VkConfig {
        access_token: SecretToken::new(token),
        base_url: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        rate_limit_rps: 20,
        ..VkConfig::default()
    })
}

/// Бэкенд логина поверх клиента состояния.
async fn login_backend(state: &Arc<forge_vk::VkState>) -> forge_vk::login::HttpBackend {
    forge_vk::login::HttpBackend::new(state.api_client().await.unwrap())
}

/// Код 5 на `users.get` → `invalid_token`, и до пробы scope дело не доходит.
///
/// Порядок проверок держится именно на этом: обратный дал бы «нет права
/// messages» на ПРОТУХШЕМ токене, и пользователь пошёл бы добывать токен с
/// другими правами вместо того, чтобы обновить старый.
#[tokio::test]
async fn login_code_five_gives_invalid_token() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = login_state(&stub, &dir, TOKEN);

    stub.state.script(
        "users.get",
        vec![Reply::Error(vk_error(5, "User authorization failed"))],
    );

    let backend = login_backend(&state).await;
    let flow = forge_vk::login::verify_token(&state, &backend, None).await;

    assert_eq!(flow.kind(), "invalid_token");
    assert_eq!(state.login_state_dto().await.state, "invalid_token");
    assert!(
        stub.state.calls_of("messages.getConversations").is_empty(),
        "протухший токен не должен доходить до пробы scope"
    );
    let error = flow.to_dto().error.unwrap_or_default();
    assert!(!error.contains(TOKEN), "токен в тексте ошибки: {error}");
    assert!(error.contains("VK error 5"), "{error}");
}

/// Код 15 виден ТОЛЬКО на `messages.*` — ради этого проба и существует.
#[tokio::test]
async fn login_code_fifteen_gives_no_messages_scope() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = login_state(&stub, &dir, TOKEN);

    stub.state.script(
        "users.get",
        vec![Reply::Response(json!([{"id": 105431859, "first_name": "Игорь"}]))],
    );
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Error(vk_error(15, "Access denied: no access to call this method"))],
    );

    let backend = login_backend(&state).await;
    let flow = forge_vk::login::verify_token(&state, &backend, None).await;

    assert_eq!(flow.kind(), "no_messages_scope");
    let error = flow.to_dto().error.unwrap_or_default();
    assert!(
        error.contains("messages"),
        "экран обязан объяснить, чего именно не хватает: {error}"
    );
    assert_eq!(stub.state.calls_of("users.get").len(), 1);
    assert_eq!(stub.state.calls_of("messages.getConversations").len(), 1);
}

/// Код 14 → `captcha_required`, а картинка доезжает до браузера ЧЕРЕЗ ПРОКСИ.
///
/// Проверяется вся цепочка: ошибка VK → состояние → запомненный адрес →
/// `GET /api/vk/captcha/:sid` → байты с `image/*` и `no-store`. Прямого
/// обращения браузера к серверам VK в ней нет ни на одном шаге.
#[tokio::test]
async fn login_code_fourteen_gives_captcha_and_the_image_goes_through_the_proxy() {
    use tower::ServiceExt;

    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = login_state(&stub, &dir, TOKEN);
    let img = format!("{}/captcha.php?sid=239123456", stub.root);

    stub.state.script(
        "users.get",
        vec![Reply::Error(json!({
            "error_code": 14,
            "error_msg": "Captcha needed",
            "captcha_sid": "239123456",
            "captcha_img": img,
            "request_params": [{"key": "access_token", "value": TOKEN}]
        }))],
    );

    let backend = login_backend(&state).await;
    let flow = forge_vk::login::verify_token(&state, &backend, None).await;
    assert_eq!(flow.kind(), "captcha_required");
    let dto = flow.to_dto();
    assert_eq!(dto.captcha_sid.as_deref(), Some("239123456"));

    // Прокси отдаёт байты картинки.
    let app = forge_vk::routes::build_router(state.clone());
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/vk/captcha/239123456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store, max-age=0"),
        "картинка одноразовая: закэшированная показала бы прошлую задачу"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG", "байты обязаны доехать без порчи");

    // Ответ на captcha = ТОТ ЖЕ запрос плюс два поля.
    stub.state.script(
        "users.get",
        vec![Reply::Response(json!([{"id": 105431859, "first_name": "Игорь"}]))],
    );
    stub.state
        .script("messages.getConversations", vec![Reply::Response(json!({"count": 0, "items": []}))]);
    let app = forge_vk::routes::build_router(state.clone());
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/vk/captcha")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"sid": "239123456", "key": "abcde"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let dto: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(dto["state"], "authorized", "{dto}");

    let calls = stub.state.calls_of("users.get");
    let last = calls.last().unwrap();
    assert_eq!(last.params.get("captcha_sid").unwrap(), "239123456");
    assert_eq!(last.params.get("captcha_key").unwrap(), "abcde");
}

/// Счастливый путь: `users.get` → проба `messages.*` → `authorized`, и
/// порядок вызовов именно такой.
#[tokio::test]
async fn login_success_authorizes_and_wakes_the_waiters() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = login_state(&stub, &dir, TOKEN);

    stub.state.script(
        "users.get",
        vec![Reply::Response(json!([{
            "id": 105431859,
            "first_name": "Игорь",
            "last_name": "Герасимов",
            "screen_name": "igor"
        }]))],
    );
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Response(json!({"count": 0, "items": []}))],
    );

    // Ждущий воркер (в Ф5 это LongPoll) обязан проснуться на переходе.
    let waiter = {
        let state = state.clone();
        tokio::spawn(async move {
            let notified = state.authorized_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            notified.await;
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;

    let backend = login_backend(&state).await;
    let flow = forge_vk::login::verify_token(&state, &backend, None).await;

    assert_eq!(flow.kind(), "authorized");
    let dto = flow.to_dto();
    let user = dto.user.expect("авторизованное состояние несёт пользователя");
    assert_eq!(user.id, "105431859");
    assert_eq!(user.display_name(), "Игорь Герасимов");

    let order: Vec<String> = stub
        .state
        .calls()
        .into_iter()
        .map(|c| c.method)
        .filter(|m| m == "users.get" || m == "messages.getConversations")
        .collect();
    assert_eq!(order, vec!["users.get", "messages.getConversations"]);

    // Проба намеренно дешёвая: без `extended=1` профили участников не нужны.
    let probe = stub.state.calls_of("messages.getConversations");
    assert_eq!(probe[0].params.get("count").unwrap(), "1");
    assert!(
        !probe[0].params.contains_key("extended"),
        "пробе не нужны профили: {:?}",
        probe[0].params
    );

    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("переход в authorized обязан будить ждущих")
        .unwrap();
}

/// ГЛАВНЫЙ ИНВАРИАНТ Ф3.4, проверенный на полном пути REST → сеть → отказ.
///
/// Пользователь вставляет АДРЕСНУЮ СТРОКУ ЦЕЛИКОМ (с `email`!), VK отвечает
/// кодом 5. Проверяем сразу три вещи:
///
/// 1. канарейка не появляется НИ В ОДНОЙ строке лога — ни от разбора, ни от
///    записи конфига, ни от транспорта, ни от обработчика ошибки;
/// 2. `email` не доезжает ни до файла, ни до логов;
/// 3. токен всё-таки СОХРАНЁН (0600) — иначе пользователь после отказа не
///    смог бы нажать «повторить», не вставляя всё заново.
#[tokio::test]
async fn login_route_keeps_the_token_and_email_out_of_the_logs() {
    use tower::ServiceExt;

    const CANARY: &str = "vk1.a.SECRET-CANARY_0123456789abcdef";
    const EMAIL: &str = "victim%40example.com";

    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = login_state(&stub, &dir, "");
    let path = dir.path().join("vk.toml");

    stub.state.script(
        "users.get",
        vec![Reply::Error(vk_error(5, "User authorization failed"))],
    );

    let lines = install_log_capture();
    let from = lines.lock().unwrap().len();

    let app = forge_vk::routes::build_router(state.clone());
    let pasted = format!(
        "https://oauth.vk.ru/blank.html#access_token={CANARY}&expires_in=0&user_id=105431859&email={EMAIL}"
    );
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/vk/login/token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "access_token": pasted }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let dto: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(dto["state"], "invalid_token", "{dto}");

    // 1. Логи.
    let mine = lines.lock().unwrap()[from..].join("\n");
    assert!(!mine.is_empty(), "путь логина обязан что-то писать в лог");
    assert!(mine.contains("users.get"), "метод в логах быть обязан:\n{mine}");
    for needle in [CANARY, "SECRET-CANARY", "0123456789abcdef", "example.com", "victim"] {
        assert!(!mine.contains(needle), "«{needle}» в логах:\n{mine}");
    }
    // И весь буфер процесса заодно — чужой тест мог наследить раньше.
    let all = lines.lock().unwrap().join("\n");
    assert!(!all.contains("SECRET-CANARY"), "канарейка в логах:\n{all}");

    // 2. Файл.
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains(CANARY), "токен обязан сохраниться для повтора");
    assert!(!body.contains("example.com"), "email в vk.toml: {body}");
    assert!(!body.contains("victim"), "email в vk.toml: {body}");
    assert!(!body.contains("user_id"), "лишние поля не пишем: {body}");

    // 3. Права.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "получили {mode:o}");
    }

    // Тело ответа тоже чисто: оно уходит в браузер и в HAR-файл поддержки.
    let rendered = dto.to_string();
    assert!(!rendered.contains("SECRET-CANARY"), "{rendered}");
    assert!(!rendered.contains("example.com"), "{rendered}");
}

/// Страница бесед из фикстуры доезжает через настоящий HTTP и разбирается
/// целиком — вместе с `profiles` и `groups`, ради которых и стоит
/// `extended=1`.
#[tokio::test]
async fn conversations_fixture_travels_over_http() {
    let stub = start_stub().await;
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/get_conversations.json")).unwrap();
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Response(fixture["response"].clone())],
    );

    let page = client(&stub, 20)
        .get_conversations(20, 0, "unread")
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.profiles.len(), 2);
    assert_eq!(page.groups.len(), 1);

    let calls = stub.state.calls_of("messages.getConversations");
    assert_eq!(calls[0].params.get("extended").unwrap(), "1");
    assert_eq!(calls[0].params.get("filter").unwrap(), "unread");
    assert_eq!(calls[0].params.get("count").unwrap(), "20");
}

// ===========================================================================
// Резолв имён (Ф4.3).
// ===========================================================================

/// Двести незнакомых id обязаны стоить ДВА запроса, а не двести: при бюджете
/// 3 rps резолв по одному означал бы, что беседа открывается минуту.
///
/// Заодно проверяется, что потолок пачки соблюдён — VK отвергает слишком
/// длинный `user_ids` кодом 100, и «работает на десяти, ломается на тысяче» —
/// худший вид отказа.
#[tokio::test]
async fn resolve_uncached_splits_ids_into_batches() {
    let stub = start_stub().await;
    let profiles: Vec<Value> = (1..=100)
        .map(|i| json!({"id": i, "first_name": format!("И{i}"), "last_name": "П"}))
        .collect();
    stub.state.script(
        "users.get",
        vec![
            Reply::Response(json!(profiles)),
            Reply::Response(json!([])),
        ],
    );

    let cache = forge_vk::peers::PeerCache::new();
    let ids: Vec<i64> = (1..=200).collect();
    cache
        .resolve_uncached(&client(&stub, 50), &ids)
        .await
        .unwrap();

    let calls = stub.state.calls_of("users.get");
    assert_eq!(calls.len(), 2, "200 id ⇒ две пачки по 100");
    for c in &calls {
        let sent = c.params.get("user_ids").unwrap();
        assert!(
            sent.split(',').count() <= forge_vk::peers::RESOLVE_BATCH,
            "пачка длиннее потолка: {}",
            sent.split(',').count()
        );
        assert!(c.params.contains_key("fields"), "поля профиля обязаны быть");
    }
    assert_eq!(cache.len(), 100, "то, что VK вернул, осело в кэше");
}

/// Повторный резолв в пределах TTL не делает НИ ОДНОГО запроса — ради этого
/// кэш и существует. Плюс сообщества уходят отдельным методом и БЕЗ знака
/// минус: с минусом VK отвечает кодом 100.
#[tokio::test]
async fn cached_names_cost_nothing_and_groups_go_unsigned() {
    let stub = start_stub().await;
    stub.state.script(
        "users.get",
        vec![Reply::Response(json!([{"id": 1234, "first_name": "Павел", "last_name": "Дуров"}]))],
    );
    stub.state.script(
        "groups.getById",
        vec![Reply::Response(json!({"groups": [{"id": 55, "name": "Клуб"}]}))],
    );

    let cache = forge_vk::peers::PeerCache::new();
    let c = client(&stub, 50);
    cache.resolve_uncached(&c, &[1234, -55]).await.unwrap();

    assert_eq!(
        stub.state.calls_of("groups.getById")[0]
            .params
            .get("group_ids")
            .unwrap(),
        "55",
        "в groups.getById id уходит БЕЗ знака минус"
    );

    // Второй заход — по кэшу, сеть не трогаем вовсе.
    for _ in 0..5 {
        cache
            .resolve_uncached(&c, &[1234, -55, 2_000_000_007])
            .await
            .unwrap();
    }
    assert_eq!(stub.state.calls_of("users.get").len(), 1);
    assert_eq!(stub.state.calls_of("groups.getById").len(), 1);
}

/// Отказ одной пачки не отменяет остальные: имена — украшение, и терять
/// из-за них страницу истории нельзя. Ошибка возвращается наверх для лога,
/// но то, что успели узнать, остаётся в кэше.
#[tokio::test]
async fn a_failed_batch_does_not_discard_the_successful_one() {
    let stub = start_stub().await;
    stub.state.script(
        "users.get",
        vec![Reply::Response(json!([{"id": 7, "first_name": "Ок"}]))],
    );
    stub.state.script(
        "groups.getById",
        vec![Reply::Error(vk_error(100, "One of the parameters is invalid"))],
    );

    let cache = forge_vk::peers::PeerCache::new();
    let err = cache
        .resolve_uncached(&client(&stub, 50), &[7, -55])
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(100));
    assert!(cache.get(7).is_some(), "удавшаяся половина обязана осесть");
    assert!(cache.get(-55).is_none());
}

// ===========================================================================
// Роуты данных (Ф4.4) — на живом HTTP-стенде.
// ===========================================================================

/// Плагин, авторизованный и настроенный на стенд.
///
/// `config_path`/`cache_dir` — в tempdir: прогон тестов не имеет права
/// тронуть `~/.config/forge/vk.toml` живого пользователя.
async fn data_app(stub: &Stub) -> (tempfile::TempDir, axum::Router) {
    use forge_vk::login::{LoginFlow, VkUserDto};
    use forge_vk::state::VkState;

    let dir = tempfile::tempdir().unwrap();
    let state = VkState::new(VkConfig {
        access_token: SecretToken::new(TOKEN),
        base_url: stub.base_url.clone(),
        rate_limit_rps: 50,
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        ..VkConfig::default()
    });
    state
        .set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "999".into(),
                first_name: Some("Я".into()),
                ..Default::default()
            },
        })
        .await;
    (dir, forge_vk::routes::build_router(state))
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Страница бесед стоит РОВНО ОДИН вызов VK: имена приезжают в блоках
/// `profiles`/`groups` того же ответа. Лишний `users.get` здесь означал бы,
/// что треть секундного бюджета уходит на данные, которые у нас уже есть.
#[tokio::test]
async fn dialogs_page_costs_a_single_vk_call() {
    let stub = start_stub().await;
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/get_conversations.json")).unwrap();
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Response(fixture["response"].clone())],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app, "/api/vk/dialogs?limit=20&filter=unread").await;
    assert_eq!(status, StatusCode::OK);
    let dialogs = json["dialogs"].as_array().unwrap();
    assert_eq!(dialogs.len(), 3);

    // Имя пришло из блока profiles того же ответа.
    assert_eq!(dialogs[0]["id"], "1234");
    assert_eq!(dialogs[0]["kind"], "user");
    assert_eq!(dialogs[0]["username"], "durov");
    assert_eq!(dialogs[0]["unread_count"], 2);
    assert_eq!(dialogs[0]["last_message"]["text"], "привет");
    // Константы формы Telegram на месте.
    assert_eq!(dialogs[0]["pinned"], false);
    assert_eq!(dialogs[0]["archived"], false);
    assert!(dialogs[0]["folder_id"].is_null());
    // Беседа подписана из chat_settings, сообщество — из блока groups.
    assert_eq!(dialogs[1]["kind"], "chat");
    assert_eq!(dialogs[1]["title"], "Проектная");
    assert_eq!(dialogs[1]["muted"], true);
    assert_eq!(dialogs[2]["title"], "Тестовое сообщество");
    assert_eq!(dialogs[2]["bot"], true);
    assert_eq!(dialogs[2]["can_write"], false);
    assert_eq!(dialogs[2]["can_write_reason"], 18);

    assert_eq!(
        stub.state.calls().len(),
        1,
        "страница бесед обязана стоить один запрос: {:?}",
        stub.state.calls().iter().map(|c| c.method.clone()).collect::<Vec<_>>()
    );
    let call = &stub.state.calls_of("messages.getConversations")[0];
    assert_eq!(call.params.get("extended").unwrap(), "1");
    assert_eq!(call.params.get("filter").unwrap(), "unread");
}

/// ГЛАВНЫЙ ТЕСТ УСТОЙЧИВОСТИ РОУТА: страница из 50 сообщений, одно из которых
/// несёт незнакомый тип вложения, отдаётся ЦЕЛИКОМ. Один странный объект не
/// имеет права стоить пользователю всей переписки.
#[tokio::test]
async fn a_page_of_fifty_survives_an_unknown_attachment() {
    let stub = start_stub().await;
    let mut items: Vec<Value> = (0..50)
        .map(|i| {
            json!({
                "id": 1000 + i,
                "peer_id": 1234,
                "date": 1_700_000_000_i64 + i,
                "from_id": 1234,
                "text": format!("сообщение {i}"),
                "conversation_message_id": 500 + i,
                "attachments": []
            })
        })
        .collect();
    items[25] = json!({
        "id": 1025,
        "peer_id": 1234,
        "date": 1_700_000_025_i64,
        "from_id": 1234,
        "text": "",
        "conversation_message_id": 525,
        "totally_new_field": {"nested": [1, 2, 3]},
        "attachments": [
            {"type": "hologram", "hologram": {"anything": true}},
            {"type": "photo", "photo": {"id": 1, "sizes": [{"url": "u", "type": "x", "width": 10, "height": 20}]}}
        ]
    });
    stub.state.script(
        "messages.getHistory",
        vec![Reply::Response(json!({"count": 500, "items": items}))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app, "/api/vk/dialogs/1234/messages?limit=50").await;
    assert_eq!(status, StatusCode::OK);
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 50, "страница отдана целиком");
    assert_eq!(json["has_more"], true);

    // Контракт наружу — возрастание id.
    let ids: Vec<i64> = messages.iter().map(|m| m["id"].as_i64().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);

    let odd = messages.iter().find(|m| m["id"] == 1025).unwrap();
    let atts = odd["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0]["kind"], "unsupported", "неизвестный вид доехал карточкой");
    assert_eq!(atts[0]["index"], 0);
    assert_eq!(atts[1]["kind"], "photo");
    assert_eq!(atts[1]["index"], 1, "индексы не сдвинулись");
    assert_eq!(odd["media"]["kind"], "unsupported", "media — ПЕРВОЕ вложение");
}

/// ГЛАВНЫЙ РАЗМЕН ВСЕГО ПЛАГИНА: страница сообщений стоит РОВНО ОДИН вызов VK
/// даже тогда, когда половина сообщений — ответы с цитатами.
///
/// У Telegram превью оригиналов добираются вторым RPC (`enrich_reply_previews`,
/// ~250 строк `dto.rs`), у VK `reply_message` инлайнится в само сообщение. Этот
/// тест — щит против возвращения дозапроса: любая попытка «добрать оригинал по
/// id» немедленно даёт второй вызов и красный CI.
///
/// Заодно проверяется, что превью действительно СОБРАНО, а не просто не
/// запрошено: пустой `reply_to` при одном запросе тоже был бы «одним запросом».
#[tokio::test]
async fn a_page_of_replies_costs_exactly_one_call() {
    let stub = start_stub().await;
    // 50 сообщений, каждое чётное — ответ на предыдущее. Половина страницы с
    // цитатами это не край, а типовая переписка.
    let items: Vec<Value> = (0..50)
        .map(|i| {
            let mut m = json!({
                "id": 1000 + i,
                "peer_id": 2000000007_i64,
                "date": 1_700_000_000_i64 + i,
                "from_id": 1234,
                "text": format!("сообщение {i}"),
                "conversation_message_id": 500 + i,
                "attachments": []
            });
            if i % 2 == 0 && i > 0 {
                m["reply_message"] = json!({
                    // Глобальный `id` у инлайненного оригинала ЕСТЬ — иначе
                    // этот тест не заметил бы возвращения дозапроса: тому
                    // просто нечего было бы добирать.
                    "id": 999 + i,
                    "date": 1_700_000_000_i64 + i - 1,
                    "from_id": 999,
                    "peer_id": 2000000007_i64,
                    "conversation_message_id": 499 + i,
                    "text": format!("оригинал {}", i - 1),
                    "attachments": []
                });
            }
            m
        })
        .collect();
    // Одно из них — ответ на сообщение БЕЗ текста, но с вложением: превью
    // обязано нести вид вложения, иначе цитата выглядела бы пустой.
    let mut items = items;
    items[11]["reply_message"] = json!({
        "date": 1_700_000_010_i64,
        "from_id": 999,
        "peer_id": 2000000007_i64,
        "conversation_message_id": 510,
        "text": "",
        "attachments": [{"type": "audio_message", "audio_message": {"duration": 3}}]
    });
    stub.state.script(
        "messages.getHistory",
        // `profiles` того же ответа закрывают имена авторов — иначе роут
        // честно доберёт их через `users.get`, и «один запрос» сорвётся не на
        // цитатах, а на никах.
        vec![Reply::Response(json!({
            "count": 500,
            "items": items,
            "profiles": [
                {"id": 1234, "first_name": "Павел", "last_name": "Дуров", "screen_name": "durov"},
                {"id": 999, "first_name": "Я", "last_name": "Сам"}
            ]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app, "/api/vk/dialogs/2000000007/messages?limit=50").await;
    assert_eq!(status, StatusCode::OK);
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 50);

    let replies: Vec<&Value> = messages
        .iter()
        .filter(|m| !m["reply_to"].is_null())
        .collect();
    assert_eq!(replies.len(), 25, "цитаты собраны у всех ответов страницы");
    // Ключ цели — cmid: именно им ответ адресуется в беседе.
    let sample = messages.iter().find(|m| m["id"] == 1004).unwrap();
    assert_eq!(sample["reply_to"]["cmid"], 503);
    assert_eq!(
        sample["reply_to"]["message_id"], 1003,
        "глобальный id оригинала едет рядом — и добирать его всё равно не нужно"
    );
    assert_eq!(sample["reply_to"]["text"], "оригинал 3");
    assert_eq!(sample["reply_to"]["unavailable"], false);
    let media_reply = messages.iter().find(|m| m["id"] == 1011).unwrap();
    assert!(media_reply["reply_to"]["text"].is_null());
    assert_eq!(
        media_reply["reply_to"]["media_kind"], "voice",
        "пустой текст при вложении даёт вид вложения, а не пустую цитату"
    );

    assert_eq!(
        stub.state.calls_of("messages.getById").len(),
        0,
        "дозапрос за оригиналами запрещён — превью приезжает инлайном"
    );
    assert_eq!(
        stub.state.calls().len(),
        1,
        "страница с ответами обязана стоить ОДИН вызов: {:?}",
        stub.state
            .calls()
            .iter()
            .map(|c| c.method.clone())
            .collect::<Vec<_>>()
    );
}

/// `messages.getById` на чужой или удалённый id отвечает не ошибкой, а пустым
/// `items` — 404 `unknown_message` порождаем МЫ. Плюс сообщение из ДРУГОЙ
/// беседы: без сверки `peer_id` роут стал бы способом читать чужую переписку,
/// подставив её id в путь.
#[tokio::test]
async fn unknown_and_foreign_messages_are_404() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Response(json!({"count": 0, "items": []})),
            Reply::Response(json!({
                "count": 1,
                "items": [{"id": 7001, "peer_id": 4321, "date": 1, "text": "чужое"}]
            })),
        ],
    );

    let (_dir, app) = data_app(&stub).await;
    let (status, json) = get_json(app, "/api/vk/dialogs/1234/messages/7001").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"]["code"], "unknown_message");

    let (_dir, app) = data_app(&stub).await;
    let (status, json) = get_json(app, "/api/vk/dialogs/1234/messages/7001").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "сообщение из другой беседы");
    assert_eq!(json["error"]["code"], "unknown_message");
}

/// Одно сообщение целиком: та же форма, что в ленте, плюс реквизиты ответа,
/// собранные из инлайненного `reply_message` БЕЗ второго запроса.
#[tokio::test]
async fn single_message_carries_the_inlined_reply() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{
                "id": 7003, "peer_id": 1234, "date": 1_700_000_120_i64, "from_id": 1234,
                "text": "это ответ", "conversation_message_id": 514,
                "reply_message": {
                    "from_id": 999, "text": "оригинал", "conversation_message_id": 513,
                    "date": 1_700_000_115_i64, "peer_id": 1234
                }
            }],
            "profiles": [
                {"id": 1234, "first_name": "Павел", "last_name": "Дуров", "screen_name": "durov"},
                {"id": 999, "first_name": "Я", "last_name": "Сам"}
            ]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app, "/api/vk/dialogs/1234/messages/7003").await;
    assert_eq!(status, StatusCode::OK);
    let m = &json["message"];
    assert_eq!(m["id"], 7003);
    assert_eq!(m["dialog_id"], "1234");
    assert_eq!(m["cmid"], 514);
    assert_eq!(m["sender"]["first_name"], "Павел");
    assert_eq!(m["reply_to"]["text"], "оригинал");
    assert_eq!(m["reply_to"]["cmid"], 513);
    assert_eq!(m["reply_to"]["unavailable"], false);

    assert_eq!(
        stub.state.calls_of("messages.getById").len(),
        1,
        "цитата приезжает инлайном — дозапроса оригинала быть не должно"
    );
    assert_eq!(
        stub.state.calls().len(),
        1,
        "все имена приехали в том же ответе — резолвить нечего: {:?}",
        stub.state.calls().iter().map(|c| c.method.clone()).collect::<Vec<_>>()
    );
}

/// Отказ VK доезжает до клиента в НАШЕМ конверте, а картинка captcha при этом
/// запоминается — иначе прокси `/api/vk/captcha/:sid` показывать было бы
/// нечего.
#[tokio::test]
async fn vk_errors_map_into_the_contract_and_remember_captchas() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getConversations",
        vec![Reply::Error(json!({
            "error_code": 14,
            "error_msg": "Captcha needed",
            "captcha_sid": "239123",
            "captcha_img": format!("{}/captcha.php?sid=239123", stub.root),
            "request_params": [{"key": "access_token", "value": TOKEN}]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app.clone(), "/api/vk/dialogs").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"]["code"], "captcha_required");
    assert_eq!(json["error"]["captcha_sid"], "239123");
    assert!(
        !serde_json::to_string(&json).unwrap().contains(TOKEN),
        "request_params обязаны быть вырезаны: {json}"
    );

    // Картинка доступна по sid через прокси — значит `sid` был запомнен.
    let (status, _) = {
        use tower::ServiceExt;
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/vk/captcha/239123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, bytes)
    };
    assert_eq!(status, StatusCode::OK, "captcha_sid из data-роута обязан быть запомнен");
}

// ===========================================================================
// Отправка, чтение, «печатает…» (Ф4.5).
// ===========================================================================

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Отправка возвращает ПОЛНЫЙ DTO, а не заглушку: фронт заменяет им
/// optimistic-строку, и объект другой формы заставил бы её мигнуть.
#[tokio::test]
async fn sending_text_returns_a_complete_message_dto() {
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7042))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{
                "id": 7042, "peer_id": 1234, "date": 1_700_000_500_i64, "from_id": 999,
                "out": 1, "text": "привет", "conversation_message_id": 600
            }],
            "profiles": [{"id": 999, "first_name": "Я", "last_name": "Сам"}]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/1234/messages",
        json!({"text": "привет"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let m = &json["message"];
    assert_eq!(m["id"], 7042);
    assert_eq!(m["dialog_id"], "1234");
    assert_eq!(m["out"], true);
    assert_eq!(m["cmid"], 600);
    assert_eq!(m["sender"]["first_name"], "Я");
    assert!(m["attachments"].as_array().unwrap().is_empty());

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(send.params.get("peer_id").unwrap(), "1234");
    assert_eq!(send.params.get("message").unwrap(), "привет");
    let random_id: i64 = send.params.get("random_id").unwrap().parse().unwrap();
    assert!(random_id > 0, "random_id=0 у VK означает «не дедуплицировать»");
}

/// Ответ адресуется `conversation_message_id`, а НЕ `message_id`.
///
/// Это не придирка к именам полей: в беседе `cmid` — единственный
/// идентификатор, одинаковый у всех участников, и именно он приезжает во
/// фронт внутри каждой цитаты. Ответ, отправленный по `message_id`, попал бы
/// в чужую ветку или не нашёл оригинала вовсе.
///
/// Мутация «слать cmid в параметр `reply_to`» краснеет здесь: `reply_to` при
/// заданном cmid обязан отсутствовать целиком.
#[tokio::test]
async fn reply_is_addressed_by_cmid_not_by_message_id() {
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7050))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{
                "id": 7050, "peer_id": 2000000007_i64, "date": 1, "from_id": 999,
                "out": 1, "text": "ответ", "conversation_message_id": 601
            }]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, _) = post_json(
        app,
        "/api/vk/dialogs/2000000007/messages",
        // Обе формы В ОДНОМ теле: точный адрес обязан победить.
        json!({"text": "ответ", "reply_to_cmid": 514, "reply_to_id": 7003}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(
        send.params.get("forward").map(String::as_str),
        Some("{\"peer_id\":2000000007,\"conversation_message_ids\":[514],\"is_reply\":1}"),
        "цель ответа едет как forward+is_reply — единственный способ адресовать cmid"
    );
    assert!(
        !send.params.contains_key("reply_to"),
        "при заданном cmid глобальный id не отправляется вовсе: {:?}",
        send.params
    );

    // Без cmid остаётся прежний путь: у сообщения из личного диалога, до
    // которого фронт добрался без cmid, других адресов нет.
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7051))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{"id": 7051, "peer_id": 1234, "date": 1, "from_id": 999, "out": 1, "text": "ответ"}]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;
    let (status, _) = post_json(
        app,
        "/api/vk/dialogs/1234/messages",
        json!({"text": "ответ", "reply_to_id": 7003}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(send.params.get("reply_to").map(String::as_str), Some("7003"));
    assert!(!send.params.contains_key("forward"));
}

/// ГЛАВНЫЙ ТЕСТ ИДЕМПОТЕНТНОСТИ РОУТА: стенд роняет первый `messages.send`,
/// принимает второй — `random_id` обязан СОВПАСТЬ. Иначе транспортный обрыв
/// превращался бы в два сообщения у собеседника.
#[tokio::test]
async fn a_transport_retry_never_duplicates_the_message() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.send",
        vec![Reply::Drop, Reply::Response(json!(7043))],
    );
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{"id": 7043, "peer_id": 1234, "date": 1, "from_id": 999, "out": 1, "text": "раз"}]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(app, "/api/vk/dialogs/1234/messages", json!({"text": "раз"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["message"]["id"], 7043);

    let calls = stub.state.calls_of("messages.send");
    assert_eq!(calls.len(), 2, "ровно один повтор");
    assert_eq!(
        calls[0].params.get("random_id"),
        calls[1].params.get("random_id"),
        "random_id при повторе обязан совпадать — по нему VK и дедуплицирует"
    );
}

/// Коды VK отображаются в контракт: 900/901/902 — «этому собеседнику писать
/// нельзя» (повтор не поможет), 914 — «текст слишком длинный» (поможет).
#[tokio::test]
async fn send_errors_map_to_distinct_contract_codes() {
    for (code, status, expected) in [
        (900, StatusCode::CONFLICT, "send_forbidden"),
        (901, StatusCode::CONFLICT, "send_forbidden"),
        (902, StatusCode::CONFLICT, "send_forbidden"),
        (914, StatusCode::BAD_REQUEST, "invalid_input"),
        (9, StatusCode::TOO_MANY_REQUESTS, "flood_wait"),
    ] {
        let stub = start_stub().await;
        stub.state.script(
            "messages.send",
            vec![Reply::Error(vk_error(code, "nope")), Reply::Error(vk_error(code, "nope"))],
        );
        let (_dir, app) = data_app(&stub).await;
        let (got, json) = post_json(
            app,
            "/api/vk/dialogs/1234/messages",
            json!({"text": "привет"}),
        )
        .await;
        assert_eq!(got, status, "код VK {code}: {json}");
        assert_eq!(json["error"]["code"], expected, "код VK {code}");
        assert!(
            !serde_json::to_string(&json).unwrap().contains(TOKEN),
            "request_params обязаны быть вырезаны: {json}"
        );
    }
}

/// Пустое сообщение и неизвестные загрузки отвергаются ДО обращения к VK:
/// тратить на них запрос из бюджета 3 rps незачем.
///
/// Коды РАЗНЫЕ и это существенно: пустой или чрезмерно длинный текст — ошибка
/// формы (`400 invalid_input`), а неизвестный `upload_id` — состояние
/// (`409 invalid_state`): файл либо уже отправлен, либо протух по TTL, и фронт
/// обязан начать загрузку заново, а не «исправить поле».
#[tokio::test]
async fn empty_text_and_unknown_uploads_are_rejected_without_touching_vk() {
    let cases = [
        (json!({}), StatusCode::BAD_REQUEST, "invalid_input"),
        (json!({"text": "   "}), StatusCode::BAD_REQUEST, "invalid_input"),
        (
            json!({"text": "я".repeat(5000)}),
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (
            json!({"text": "привет", "upload_id": "u1"}),
            StatusCode::CONFLICT,
            "invalid_state",
        ),
        (
            json!({"text": "привет", "upload_ids": ["u1", "u2"]}),
            StatusCode::CONFLICT,
            "invalid_state",
        ),
    ];
    for (body, status_expected, expected) in cases {
        let stub = start_stub().await;
        let (_dir, app) = data_app(&stub).await;
        let (status, json) = post_json(app, "/api/vk/dialogs/1234/messages", body.clone()).await;
        assert_eq!(status, status_expected, "{body}: {json}");
        assert_eq!(json["error"]["code"], expected, "{body}");
        assert!(stub.state.calls().is_empty(), "запрос к VK не должен был случиться: {body}");
    }
}

/// `read` и `typing` бьют в свои методы VK и передают параметры как есть.
#[tokio::test]
async fn read_and_typing_hit_their_vk_methods() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    let (status, json) = post_json(app, "/api/vk/dialogs/1234/read", json!({"max_id": 7001})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], true);
    let call = &stub.state.calls_of("messages.markAsRead")[0];
    assert_eq!(call.params.get("peer_id").unwrap(), "1234");
    assert_eq!(call.params.get("start_message_id").unwrap(), "7001");

    let (_dir, app) = data_app(&stub).await;
    let (status, _) = post_json(
        app,
        "/api/vk/dialogs/2000000007/typing",
        json!({"action": "audiomessage"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let call = &stub.state.calls_of("messages.setActivity")[0];
    assert_eq!(call.params.get("peer_id").unwrap(), "2000000007");
    assert_eq!(call.params.get("type").unwrap(), "audiomessage");

    // Незнакомый вид активности приводится к `typing`, а не улетает в VK
    // кодом 100.
    let (_dir, app) = data_app(&stub).await;
    let _ = post_json(app, "/api/vk/dialogs/1234/typing", json!({"action": "dancing"})).await;
    let calls = stub.state.calls_of("messages.setActivity");
    assert_eq!(calls.last().unwrap().params.get("type").unwrap(), "typing");
}

/// Ответ на captcha подставляется в параметры VK КАК ЕСТЬ — это и есть вся
/// механика повтора после кода 14.
#[tokio::test]
async fn captcha_answer_is_passed_through_verbatim() {
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7050))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{"id": 7050, "peer_id": 1234, "date": 1, "from_id": 999, "out": 1, "text": "ок"}]
        }))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, _) = post_json(
        app,
        "/api/vk/dialogs/1234/messages",
        json!({"text": "ок", "captcha_sid": "239123", "captcha_key": "xyz"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let call = &stub.state.calls_of("messages.send")[0];
    assert_eq!(call.params.get("captcha_sid").unwrap(), "239123");
    assert_eq!(call.params.get("captcha_key").unwrap(), "xyz");
}

/// Сообщение ушло, а добор полного DTO не удался: отдавать 500 нельзя — оно
/// уже у собеседника. Клиент получает минимальный DTO с настоящим id.
#[tokio::test]
async fn a_sent_message_is_never_reported_as_a_failure() {
    let stub = start_stub().await;
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7099))]);
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Error(vk_error(10, "Internal server error")),
            Reply::Error(vk_error(10, "Internal server error")),
        ],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/1234/messages",
        json!({"text": "доехало"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["message"]["id"], 7099);
    assert_eq!(json["message"]["text"], "доехало");
    assert_eq!(json["message"]["out"], true);
}

// ===========================================================================
// Ф5 — LongPoll-воркер на стенде.
//
// Восстановление после `failed` — единственная часть realtime, которую в живом
// виде можно воспроизвести только «подождав сутки»: `failed:1` приходит, когда
// протух `ts`, `failed:2` — когда протух ключ, `failed:3` — когда VK потерял
// историю. Именно на них держится непрерывность потока, поэтому проверяются
// они здесь, стендом, и без единого секрета.
// ===========================================================================

/// Состояние плагина, направленное на стенд, с КОРОТКИМИ таймингами воркера.
///
/// `TempDir` возвращается наружу: он владеет каталогом кэша, куда воркер пишет
/// курсор, и, будучи брошенным в кадре хелпера, снёс бы его на первом же
/// `save_cursor`.
async fn lp_state(stub: &Stub) -> (tempfile::TempDir, Arc<forge_vk::state::VkState>) {
    lp_state_tuned(stub, Duration::from_millis(20)).await
}

/// То же, но с явным периодом перепроверки авторизации.
///
/// Отдельная ручка нужна ровно одному сценарию — «токен вставили в работающем
/// процессе». Тик `auth_poll` там обязан быть ЗАВЕДОМО больше времени теста:
/// иначе воркер поднимется по нему, а не по уведомлению, и тест молча
/// перестанет проверять анти-гонку `notified().enable()` — то есть будет
/// зелёным при сломанном коде.
async fn lp_state_tuned(
    stub: &Stub,
    auth_poll: Duration,
) -> (tempfile::TempDir, Arc<forge_vk::state::VkState>) {
    use forge_vk::login::{LoginFlow, VkUserDto};
    use forge_vk::longpoll::LpTimings;
    use forge_vk::state::VkState;

    let dir = tempfile::tempdir().unwrap();
    let state = VkState::with_timings(
        VkConfig {
            access_token: SecretToken::new(TOKEN),
            base_url: stub.base_url.clone(),
            rate_limit_rps: 50,
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            ..VkConfig::default()
        },
        Duration::from_millis(50),
        Duration::from_millis(500),
        LpTimings {
            auth_poll,
            // Лестница из миллисекунд: проверяется ЛОГИКА повторов, а не
            // выдержка. С боевыми [1,2,5,10,30,60] сценарий шёл бы минуту.
            backoff: vec![Duration::from_millis(5), Duration::from_millis(10)],
            // `wait` доезжает до стенда и определяет, сколько тот держит
            // соединение, когда сценарий доигран.
            wait_secs: 1,
            persist_every: 1,
        },
    );
    state
        .set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "999".into(),
                ..Default::default()
            },
        })
        .await;
    (dir, state)
}

/// Ответ `messages.getLongPollServer` от стенда.
fn lp_server_reply(stub: &Stub, key: &str, ts: u64, pts: u64) -> Reply {
    Reply::Response(json!({
        "server": stub.lp_server,
        "key": key,
        "ts": ts,
        "pts": pts,
    }))
}

/// Собирает кадры из broadcast'а, пока не наберётся `want` штук.
///
/// Ждёт по событию, а не «спит и смотрит»: сон, подобранный под скорость
/// машины, — главный источник мигающих тестов.
async fn collect_frames(
    rx: &mut tokio::sync::broadcast::Receiver<forge_vk::VkServerMsg>,
    want: usize,
    within: Duration,
) -> Vec<forge_vk::VkServerMsg> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(within, async {
        while out.len() < want {
            match rx.recv().await {
                Ok(frame) => out.push(frame),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    })
    .await;
    out
}

/// Ждёт кадра, удовлетворяющего предикату, пропуская всё остальное.
///
/// Нужен там, где по каналу идут ещё и кадры `login_state`: подписка,
/// открытая ДО перехода автомата, поймает их первыми, и «сравнение по
/// позиции» стало бы проверкой очерёдности вместо проверки факта.
async fn wait_for_frame<F: Fn(&forge_vk::VkServerMsg) -> bool>(
    rx: &mut tokio::sync::broadcast::Receiver<forge_vk::VkServerMsg>,
    within: Duration,
    matches: F,
) -> Option<forge_vk::VkServerMsg> {
    tokio::time::timeout(within, async {
        loop {
            match rx.recv().await {
                Ok(frame) if matches(&frame) => return Some(frame),
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Ждёт, пока предикат станет истинным (или истечёт срок).
async fn wait_until<F: Fn() -> bool>(within: Duration, check: F) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}

/// ГЛАВНЫЙ сценарий фазы: `updates` → `failed:2` → новый ключ → снова
/// `updates`.
///
/// Проверяется ровно два инварианта, и оба невозможно проверить иначе:
///
/// 1. **не потерян ни один кадр** — оба события доехали до подписчиков;
/// 2. **`ts` НЕ сброшен** — следующий `a_check` после смены ключа пришёл с
///    нашим `ts`, а не с тем, что выдал свежий `getLongPollServer`.
///
/// Второй — тот самый, ради которого `recovery_for(ExpiredKey)` возвращает
/// `RenewKeyKeepTs`: пока ключ протухал, на сервере копились события, и `ts`
/// из свежего ответа указывает на «сейчас». Взяв его, мы молча потеряли бы всё
/// накопленное — а заметно это стало бы только у пользователя.
#[tokio::test]
async fn expired_key_renews_the_key_without_losing_frames_or_resetting_ts() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![
            lp_server_reply(&stub, "KEY1", 100, 5),
            // `ts` НАРОЧНО далёкий: если воркер возьмёт его, это увидит ассерт.
            lp_server_reply(&stub, "KEY2", 500, 9),
        ],
    );
    stub.state.lp_script(vec![
        json!({"ts": 101, "updates": [[80, 7, 0]]}),
        json!({"failed": 2}),
        json!({"ts": 102, "updates": [[80, 9, 0]]}),
    ]);

    let (_dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;

    let frames = collect_frames(&mut rx, 2, Duration::from_secs(5)).await;
    assert_eq!(
        frames,
        vec![
            forge_vk::VkServerMsg::UnreadTotal { count: 7 },
            forge_vk::VkServerMsg::UnreadTotal { count: 9 },
        ],
        "после failed:2 не должно потеряться ни одного кадра"
    );

    let ts = stub.state.lp_param("ts");
    let key = stub.state.lp_param("key");
    assert_eq!(&ts[..3], ["100", "101", "101"], "ts обязан пережить смену ключа: {ts:?}");
    assert_eq!(&key[..3], ["KEY1", "KEY1", "KEY2"], "ключ обязан обновиться: {key:?}");
    assert_ne!(ts[2], "500", "ts из свежего getLongPollServer брать НЕЛЬЗЯ");
    assert_eq!(stub.state.calls_of("messages.getLongPollServer").len(), 2);

    // `failed` — это НЕ ошибка, а штатная часть протокола: восстановление
    // обязано пройти без паузы backoff'а. Иначе смена ключа раз в сутки
    // оборачивалась бы минутой тишины в realtime.
    let calls = stub.state.lp_calls();
    let span = calls[2].at.duration_since(calls[0].at);
    assert!(
        span < Duration::from_secs(1),
        "восстановление после failed:2 ушло в backoff, заняло {span:?}"
    );

    forge_vk::shutdown(&state).await;
}

/// `failed:1`: `ts` берётся ИЗ ОТВЕТА, ключ остаётся прежним — то есть
/// `getLongPollServer` не зовётся вовсе.
#[tokio::test]
async fn stale_ts_is_taken_from_the_response_and_the_key_survives() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![lp_server_reply(&stub, "KEY1", 100, 5)],
    );
    stub.state.lp_script(vec![
        json!({"ts": 101, "updates": [[80, 1, 0]]}),
        json!({"failed": 1, "ts": 777}),
        json!({"ts": 778, "updates": [[80, 2, 0]]}),
    ]);

    let (_dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;

    let frames = collect_frames(&mut rx, 2, Duration::from_secs(5)).await;
    assert_eq!(
        frames,
        vec![
            forge_vk::VkServerMsg::UnreadTotal { count: 1 },
            forge_vk::VkServerMsg::UnreadTotal { count: 2 },
        ]
    );

    let ts = stub.state.lp_param("ts");
    assert_eq!(&ts[..3], ["100", "101", "777"], "ts обязан приехать из ответа: {ts:?}");
    assert!(
        stub.state.lp_param("key").iter().all(|k| k == "KEY1"),
        "ключ при failed:1 не меняется"
    );
    assert_eq!(
        stub.state.calls_of("messages.getLongPollServer").len(),
        1,
        "failed:1 не требует нового ключа"
    );

    forge_vk::shutdown(&state).await;
}

/// `failed:3`: курсор сбрасывается ЦЕЛИКОМ (и ключ, и `ts`), клиентам уходит
/// `resync` — история на сервере оборвана, и догонять нечего.
#[tokio::test]
async fn lost_history_resets_the_cursor_and_broadcasts_resync() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![
            lp_server_reply(&stub, "KEY1", 100, 5),
            lp_server_reply(&stub, "KEY3", 900, 11),
        ],
    );
    stub.state.lp_script(vec![
        json!({"failed": 3}),
        json!({"ts": 901, "updates": [[80, 3, 0]]}),
    ]);

    let (_dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;

    let frames = collect_frames(&mut rx, 2, Duration::from_secs(5)).await;
    assert_eq!(
        frames,
        vec![
            forge_vk::VkServerMsg::Resync,
            forge_vk::VkServerMsg::UnreadTotal { count: 3 },
        ],
        "failed:3 обязан сначала сказать клиенту «перечитай всё»"
    );

    let ts = stub.state.lp_param("ts");
    let key = stub.state.lp_param("key");
    assert_eq!(&ts[..2], ["100", "900"], "курсор сброшен целиком: {ts:?}");
    assert_eq!(&key[..2], ["KEY1", "KEY3"]);

    forge_vk::shutdown(&state).await;
}

/// Пачка из десяти новых сообщений обязана стоить РОВНО ОДИН запрос.
///
/// Это и есть цена решения в пользу `messages.getLongPollHistory`: при
/// отвергнутом `getById`-на-сообщение та же пачка съела бы больше трёх секунд
/// бюджета 3 rps и начала бы генерировать код 6, задерживая интерактивные
/// запросы пользователя.
#[tokio::test]
async fn a_batch_of_ten_messages_costs_exactly_one_enrichment_request() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![lp_server_reply(&stub, "KEY1", 100, 5)],
    );

    let updates: Vec<Value> = (0..10)
        .map(|i| json!([4, 7000 + i, 17, 1234, 1_800_000_000, "привет"]))
        .collect();
    stub.state
        .lp_script(vec![json!({"ts": 101, "updates": updates})]);

    let items: Vec<Value> = (0..10)
        .map(|i| json!({"id": 7000 + i, "peer_id": 1234, "date": 1_800_000_000, "from_id": 1234, "text": "привет"}))
        .collect();
    stub.state.script(
        "messages.getLongPollHistory",
        vec![Reply::Response(json!({
            "history": [],
            "messages": {"count": 10, "items": items},
            "profiles": [{"id": 1234, "first_name": "Пётр", "last_name": "Петров"}],
            "groups": [],
            "conversations": [{"peer": {"id": 1234, "type": "user"}}],
            "new_pts": 777
        }))],
    );

    let (_dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;

    let frames = collect_frames(&mut rx, 10, Duration::from_secs(5)).await;
    assert_eq!(frames.len(), 10, "десять событий — десять кадров");
    assert!(
        frames.iter().all(|f| matches!(f, forge_vk::VkServerMsg::NewMessage { .. })),
        "все кадры обязаны быть new_message: {frames:?}"
    );

    assert_eq!(
        stub.state.calls_of("messages.getLongPollHistory").len(),
        1,
        "ОДИН запрос на пачку — иначе решение фазы нарушено"
    );
    assert!(
        stub.state.calls_of("messages.getById").is_empty(),
        "getById на сообщение допустим ТОЛЬКО в ветке деградации"
    );

    // Имена приехали вместе с историей — второго запроса за ними нет.
    assert!(
        stub.state.calls_of("users.get").is_empty(),
        "profiles из getLongPollHistory закрывают резолв имён бесплатно"
    );

    forge_vk::shutdown(&state).await;
    // `new_pts` из ответа стал курсором — это и есть тёплый догон при
    // переподключении внутри процесса.
    assert_eq!(state.lp_cursor().await.and_then(|c| c.pts), Some(777));
}

/// Деградация: `getLongPollHistory` отвечает кодом 10 (курсор старше суток).
///
/// Тогда и только тогда разрешён отвергнутый путь «`getById` на сообщение», и
/// он ограничен `MAX_FALLBACK_GETBYID` штуками. Всё, что не добрали,
/// закрывается кадром `resync`: перечитать состояние по REST дешевле, чем
/// доставлять каждое сообщение отдельным запросом.
#[tokio::test]
async fn enrichment_failure_degrades_to_three_get_by_id_and_resync() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![lp_server_reply(&stub, "KEY1", 100, 5)],
    );

    let updates: Vec<Value> = (0..5)
        .map(|i| json!([4, 7100 + i, 17, 1234, 1_800_000_000, "привет"]))
        .collect();
    stub.state
        .lp_script(vec![json!({"ts": 101, "updates": updates})]);

    // Код 10 повторяется один раз политикой ретраев — значит в скрипте два.
    stub.state.script(
        "messages.getLongPollHistory",
        vec![
            Reply::Error(vk_error(10, "Internal server error")),
            Reply::Error(vk_error(10, "Internal server error")),
        ],
    );
    let items: Vec<Value> = (0..3)
        .map(|i| json!({"id": 7100 + i, "peer_id": 1234, "date": 1_800_000_000, "from_id": 1234, "text": "привет"}))
        .collect();
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({"count": 3, "items": items}))],
    );

    let (_dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;

    let frames = collect_frames(&mut rx, 5, Duration::from_secs(10)).await;
    let new_messages = frames
        .iter()
        .filter(|f| matches!(f, forge_vk::VkServerMsg::NewMessage { .. }))
        .count();
    let resyncs = frames
        .iter()
        .filter(|f| matches!(f, forge_vk::VkServerMsg::Resync))
        .count();
    assert_eq!(new_messages, 3, "добрано ровно MAX_FALLBACK_GETBYID: {frames:?}");
    assert_eq!(resyncs, 2, "остаток закрывается resync: {frames:?}");

    let by_id = stub.state.calls_of("messages.getById");
    assert_eq!(by_id.len(), 1, "деградация стоит ОДИН запрос, а не пять");
    assert_eq!(
        by_id[0].params.get("message_ids").unwrap(),
        "7100,7101,7102",
        "потолок деградации обязан соблюдаться"
    );

    forge_vk::shutdown(&state).await;
}

/// Токен вставили в РАБОТАЮЩЕМ процессе: воркер, стоявший на
/// `authorized_notify`, обязан подняться сам, без рестарта devforge.
///
/// Гоняется в цикле: гонка «токен вставили между проверкой и подпиской»
/// воспроизводится не с первой попытки, а `notified().enable()` ДО проверки —
/// единственное, что её закрывает.
#[tokio::test]
async fn a_token_inserted_at_runtime_brings_realtime_up_without_a_restart() {
    for attempt in 0..20 {
        let stub = start_stub().await;
        stub.state.script(
            "messages.getLongPollServer",
            vec![lp_server_reply(&stub, "KEY1", 100, 5)],
        );
        stub.state
            .lp_script(vec![json!({"ts": 101, "updates": [[80, 42, 0]]})]);

        // Тик перепроверки заведомо длиннее теста: единственный путь наверх —
        // уведомление. Иначе мутация «подписаться ПОСЛЕ проверки» выжила бы.
        let (_dir, state) = lp_state_tuned(&stub, Duration::from_secs(30)).await;
        // Стартуем БЕЗ авторизации — ровно так выглядит свежий процесс.
        state
            .set_login(forge_vk::login::LoginFlow::NotConfigured)
            .await;
        let mut rx = state.subscribe();
        forge_vk::longpoll::spawn(&state).await;

        // Пауза подобрана так, чтобы попасть в окно между проверкой состояния
        // и подпиской на уведомление.
        tokio::time::sleep(Duration::from_micros(200 * attempt + 50)).await;
        state
            .set_login(forge_vk::login::LoginFlow::Authorized {
                user: forge_vk::login::VkUserDto {
                    id: "999".into(),
                    ..Default::default()
                },
            })
            .await;

        // Ждём ИМЕННО кадр события, пропуская `login_state` от самого
        // перехода: «собрать N кадров и посмотреть» уперлось бы в тайм-аут на
        // каждой из двадцати попыток и растянуло бы тест на минуту.
        let got = wait_for_frame(&mut rx, Duration::from_secs(2), |f| {
            matches!(f, forge_vk::VkServerMsg::UnreadTotal { count: 42 })
        })
        .await;
        assert!(
            got.is_some(),
            "попытка {attempt}: realtime не поднялся после вставки токена"
        );
        forge_vk::shutdown(&state).await;
    }
}

/// Код 5 (токен отозван): автомат логина уходит в `invalid_token`, и
/// РЕКОННЕКТА НЕТ — воркер не имеет права долбить VK мёртвым токеном.
#[tokio::test]
async fn a_revoked_token_stops_the_worker_without_reconnecting() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![
            Reply::Error(vk_error(5, "User authorization failed")),
            // Второй ответ приготовлен НАРОЧНО: если воркер пойдёт на
            // реконнект, счётчик вызовов это покажет.
            lp_server_reply(&stub, "KEY1", 100, 5),
        ],
    );

    let (_dir, state) = lp_state(&stub).await;
    forge_vk::longpoll::spawn(&state).await;

    assert!(
        wait_until(Duration::from_secs(3), || {
            !stub.state.calls_of("messages.getLongPollServer").is_empty()
        })
        .await
    );
    // Даём воркеру заведомо больше времени, чем его backoff (5-10 мс).
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(state.login_state_dto().await.state, "invalid_token");
    assert_eq!(
        stub.state.calls_of("messages.getLongPollServer").len(),
        1,
        "мёртвый токен не повторяют: следующий виток только после нового"
    );

    forge_vk::shutdown(&state).await;
}

/// Код 15 (у токена нет права `messages`): свой экран и тоже без реконнекта.
#[tokio::test]
async fn a_token_without_the_messages_scope_stops_the_worker() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![
            Reply::Error(vk_error(15, "Access denied: no access to call this method")),
            lp_server_reply(&stub, "KEY1", 100, 5),
        ],
    );

    let (_dir, state) = lp_state(&stub).await;
    forge_vk::longpoll::spawn(&state).await;

    assert!(
        wait_until(Duration::from_secs(3), || {
            !stub.state.calls_of("messages.getLongPollServer").is_empty()
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(state.login_state_dto().await.state, "no_messages_scope");
    assert_eq!(stub.state.calls_of("messages.getLongPollServer").len(), 1);

    forge_vk::shutdown(&state).await;
}

/// Курсор переживает «рестарт процесса»: второй воркер продолжает с той же
/// позиции, а не запрашивает свежую.
///
/// Именно поэтому `shutdown_workers` даёт воркеру время выйти самому: `abort()`
/// в произвольной await-точке не дал бы записать курсор, и поток после
/// рестарта начался бы «с сейчас» с потерей накопленного.
#[tokio::test]
async fn the_cursor_survives_a_process_restart() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getLongPollServer",
        vec![lp_server_reply(&stub, "KEY1", 100, 5)],
    );
    stub.state
        .lp_script(vec![json!({"ts": 150, "updates": [[80, 1, 0]]})]);

    let (dir, state) = lp_state(&stub).await;
    let mut rx = state.subscribe();
    forge_vk::longpoll::spawn(&state).await;
    let _ = collect_frames(&mut rx, 1, Duration::from_secs(5)).await;
    forge_vk::shutdown(&state).await;

    let path = forge_vk::longpoll::cursor_path(&state.config);
    let stored = forge_vk::longpoll::load_cursor(&path, chrono::Utc::now().timestamp())
        .expect("курсор обязан быть на диске");
    assert_eq!(stored.ts, 150);
    assert_eq!(stored.key, "KEY1");
    assert_eq!(stored.pts, Some(5));

    // Второй запуск с тем же cache_dir: свежий курсор НЕ запрашивается.
    stub.state.script("messages.getLongPollServer", vec![]);
    let calls_before = stub.state.calls_of("messages.getLongPollServer").len();
    let state2 = forge_vk::state::VkState::with_timings(
        forge_vk::config::VkConfig {
            access_token: SecretToken::new(TOKEN),
            base_url: stub.base_url.clone(),
            rate_limit_rps: 50,
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            ..VkConfig::default()
        },
        Duration::from_millis(50),
        Duration::from_millis(500),
        forge_vk::longpoll::LpTimings {
            auth_poll: Duration::from_millis(20),
            backoff: vec![Duration::from_millis(5)],
            wait_secs: 1,
            persist_every: 1,
        },
    );
    state2
        .set_login(forge_vk::login::LoginFlow::Authorized {
            user: forge_vk::login::VkUserDto {
                id: "999".into(),
                ..Default::default()
            },
        })
        .await;
    forge_vk::longpoll::spawn(&state2).await;

    assert!(
        wait_until(Duration::from_secs(3), || {
            stub.state.lp_param("ts").len() > 1
        })
        .await
    );
    assert_eq!(
        stub.state.calls_of("messages.getLongPollServer").len(),
        calls_before,
        "сохранённый курсор избавляет от запроса за новым"
    );
    assert_eq!(
        stub.state.lp_param("ts").last().map(String::as_str),
        Some("150"),
        "поток продолжается с сохранённой позиции"
    );

    forge_vk::shutdown(&state2).await;
}

// ===========================================================================
// Ф7. Медиа: дедуп двух уровней, кэш, протухшая ссылка.
// ===========================================================================

/// Ответ `messages.getById` с сообщением из `count` фотографий.
///
/// У каждого фото ТРИ размера и свой адрес на «CDN» стенда. Имя адреса —
/// `<байт>-<индекс>-<буква>`: размер файла различает выбранный вариант (64 —
/// мелкий `s`, 128 — превью `p`, 4096 — оригинал `w`), хвост различает
/// вложения. Если роут перепутает индекс или вид, это видно по содержимому
/// ответа, а не только по счётчику вызовов.
fn gallery_reply_on(stub: &Stub, peer_id: i64, message_id: i64, count: usize) -> Value {
    let atts: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "type": "photo",
                "photo": {
                    "id": 1000 + i as i64,
                    "owner_id": 1,
                    "sizes": [
                        {"url": format!("{}/64-{i}-s", stub.cdn_url), "width": 75, "height": 50, "type": "s"},
                        {"url": format!("{}/128-{i}-p", stub.cdn_url), "width": 200, "height": 133, "type": "p"},
                        {"url": format!("{}/4096-{i}-w", stub.cdn_url), "width": 2560, "height": 1707, "type": "w"},
                    ],
                },
            })
        })
        .collect();
    json!({
        "count": 1,
        "items": [{
            "id": message_id,
            "peer_id": peer_id,
            "conversation_message_id": 1,
            "from_id": 1,
            "date": 1_800_000_000,
            "text": "галерея",
            "attachments": atts,
        }],
        "profiles": [],
        "groups": [],
    })
}

/// Байты ответа роута.
async fn get_raw(app: axum::Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

/// ГЕЙТ ФАЗЫ: галерея из десяти вложений одного сообщения стоит РОВНО ОДНОГО
/// `messages.getById`.
///
/// Это и есть второй уровень дедупа. Без него десять карточек выстрелили бы
/// десятью одинаковыми запросами, а при бюджете 3 rps это 3+ секунды
/// троттлинга на одну картинку — то есть лента с фотоальбомом «думает» вместо
/// того, чтобы рисоваться.
///
/// МУТАЦИОННАЯ МИШЕНЬ: снять дедуп по сообщению ⇒ счётчик вырастет до 10.
#[tokio::test]
async fn a_gallery_costs_exactly_one_get_by_id() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    // Один заготовленный ответ на ВСЕ десять запросов: второй `getById`
    // получил бы дефолтный `{"response":1}` и уронил бы разбор — то есть
    // нарушение инварианта видно даже без счётчика.
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(gallery_reply_on(
            &stub,
            2_000_000_001,
            7001,
            10,
        ))],
    );

    let mut tasks = Vec::new();
    for i in 0..10 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            get_raw(
                app,
                &format!("/api/vk/media/2000000001/7001/{i}?kind=full"),
            )
            .await
        }));
    }
    for (i, t) in tasks.into_iter().enumerate() {
        let (status, headers, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK, "вложение {i}");
        assert_eq!(body.len(), 4096, "вложение {i} отдано целиком");
        assert_eq!(headers[axum::http::header::CONTENT_TYPE], "image/jpeg");
    }

    assert_eq!(
        stub.state.calls_of("messages.getById").len(),
        1,
        "галерея обязана стоить ОДНОГО getById"
    );
    // А вот файлов скачано десять — по одному на вложение, и все разные.
    let cdn = stub.state.cdn_calls();
    assert_eq!(cdn.len(), 10, "{cdn:?}");
    let mut sorted = cdn.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 10, "адреса вложений не должны совпадать");
}

/// Попадание в кэш не стоит НИ ОДНОГО обращения к VK — ни к API, ни к CDN.
#[tokio::test]
async fn a_cached_attachment_never_touches_vk_again() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(gallery_reply_on(&stub, 777, 42, 1))],
    );

    let (status, _, first) = get_raw(app.clone(), "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stub.state.calls_of("messages.getById").len(), 1);
    assert_eq!(stub.state.cdn_calls().len(), 1);

    // Второй запрос — уже с диска. Скрипт `getById` при этом ПУСТ: сходи роут
    // в VK, он получил бы `{"response":1}` и не смог бы разобрать страницу.
    let (status, headers, second) = get_raw(app, "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second, first, "из кэша отдаются те же байты");
    assert_eq!(
        headers[axum::http::header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(
        stub.state.calls_of("messages.getById").len(),
        1,
        "кэш-попадание не ходит в VK"
    );
    assert_eq!(stub.state.cdn_calls().len(), 1, "и на CDN тоже не ходит");
}

/// `thumb` и `full` — независимые записи и разные файлы.
#[tokio::test]
async fn thumb_and_full_are_downloaded_separately() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
        ],
    );

    let (s1, _, thumb) = get_raw(app.clone(), "/api/vk/media/777/42/0?kind=thumb").await;
    let (s2, _, full) = get_raw(app, "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_eq!(thumb.len(), 128, "превью — минимальный подходящий размер");
    assert_eq!(full.len(), 4096, "оригинал — максимальный");
}

/// ГЕЙТ ФАЗЫ: протухшая подписанная ссылка даёт РОВНО ОДИН повтор с новым
/// адресом, после чего — честный `404 no_media`.
///
/// Именно так работает инвариант «URL не кэшируются никогда»: адрес добывается
/// заново, а не берётся из памяти.
#[tokio::test]
async fn an_expired_link_is_retried_once_and_then_reported() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
        ],
    );
    // CDN отвергает ссылку всегда.
    stub.state.cdn_script(vec![403, 403, 403]);

    let (status, _, body) = get_raw(app, "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "no_media");
    assert_eq!(
        stub.state.cdn_calls().len(),
        2,
        "одна попытка + РОВНО один повтор"
    );
    assert_eq!(
        stub.state.calls_of("messages.getById").len(),
        2,
        "повтор обязан идти с ЗАНОВО добытым адресом"
    );
}

/// Тот же сценарий, но CDN отдаёт файл со второй попытки: пользователь
/// получает картинку, а не ошибку.
#[tokio::test]
async fn an_expired_link_recovers_on_the_second_try() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
            Reply::Response(gallery_reply_on(&stub, 777, 42, 1)),
        ],
    );
    stub.state.cdn_script(vec![404]);

    let (status, _, body) = get_raw(app, "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 4096);
    assert_eq!(stub.state.cdn_calls().len(), 2);
}

/// Чужое сообщение по подобранному номеру недоступно: `getById` адресуется
/// глобальным id, и без сверки беседы маршрут отдавал бы вложения из чужой
/// переписки.
#[tokio::test]
async fn an_attachment_from_another_dialog_is_404() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(gallery_reply_on(&stub, 777, 42, 1))],
    );

    let (status, _, body) = get_raw(app, "/api/vk/media/999/42/0?kind=full").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "unknown_message");
    assert!(stub.state.cdn_calls().is_empty(), "качать было нечего");
}

/// Вложения с таким индексом нет — `404 no_media`, и ни одного похода на CDN.
#[tokio::test]
async fn a_missing_attachment_index_is_no_media() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(gallery_reply_on(&stub, 777, 42, 2))],
    );

    let (status, _, body) = get_raw(app, "/api/vk/media/777/42/5?kind=full").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "no_media");
    assert!(stub.state.cdn_calls().is_empty());
}

/// У видео нет файла — `full` честно отвечает `no_media`, а превью работает.
#[tokio::test]
async fn a_video_has_a_preview_but_no_file() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    let reply = json!({
        "count": 1,
        "items": [{
            "id": 42,
            "peer_id": 777,
            "conversation_message_id": 1,
            "from_id": 1,
            "date": 1_800_000_000,
            "text": "",
            "attachments": [{
                "type": "video",
                "video": {
                    "id": 1,
                    "owner_id": 1,
                    "title": "ролик",
                    "duration": 95,
                    "player": "https://vk.com/video_ext.php?oid=1",
                    "image": [
                        {"url": format!("{}/256-v-l", stub.cdn_url), "width": 320, "height": 180, "type": "l"},
                    ],
                },
            }],
        }],
        "profiles": [],
        "groups": [],
    });
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(reply.clone()), Reply::Response(reply)],
    );

    let (status, _, body) = get_raw(app.clone(), "/api/vk/media/777/42/0?kind=thumb").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), 256);

    let (status, _, body) = get_raw(app, "/api/vk/media/777/42/0?kind=full").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "видеофайла у вложения нет");
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "no_media");
}

/// Сообщение со всеми видами вложений разом — ОДНА И ТА ЖЕ форма для
/// `messages.getHistory` и `messages.getById`.
///
/// Форма общая НАМЕРЕННО: VK отдаёт объект сообщения одинаковым обоими
/// методами, и тест `every_promised_thumb_is_actually_downloadable` опирается
/// именно на это. Ленте `getHistory` рисует карточки (и обещает превью), а
/// роут медиа перед КАЖДЫМ скачиванием резолвит сообщение заново через
/// `getById` — если бы формы расходились, обещание и способность его выполнить
/// разъехались бы уже на транспорте.
///
/// Размер файла на «CDN» стенда закодирован в имени адреса, поэтому по длине
/// ответа видно, какой именно вариант выбрал роут.
fn every_kind_message(stub: &Stub, peer_id: i64, message_id: i64) -> Value {
    json!({
        "count": 1,
        "items": [{
            "id": message_id,
            "peer_id": peer_id,
            "conversation_message_id": 1,
            "from_id": 1,
            "date": 1_800_000_000,
            "text": "всё сразу",
            "attachments": [
                {"type": "photo", "photo": {"id": 1, "owner_id": 1, "sizes": [
                    {"url": format!("{}/64-photo-s", stub.cdn_url), "width": 75, "height": 50, "type": "s"},
                    {"url": format!("{}/128-photo-p", stub.cdn_url), "width": 200, "height": 133, "type": "p"},
                    {"url": format!("{}/4096-photo-w", stub.cdn_url), "width": 2560, "height": 1707, "type": "w"},
                ]}},
                {"type": "doc", "doc": {"id": 2, "title": "смета.pdf", "ext": "pdf", "size": 4096,
                    "url": format!("{}/2048-doc", stub.cdn_url),
                    "preview": {"photo": {"sizes": [
                        {"url": format!("{}/512-doc-p", stub.cdn_url), "width": 200, "height": 300, "type": "m"},
                    ]}}}},
                // ↓ ТОТ САМЫЙ ДЕФЕКТ: ссылка с превью 1200×630.
                {"type": "link", "link": {
                    "url": "https://psph.example/crm/contact/details/27307/",
                    "title": "Войти в Битрикс24",
                    "description": "CRM",
                    "photo": {"id": 3, "owner_id": 1, "sizes": [
                        {"url": format!("{}/96-link-m", stub.cdn_url), "width": 130, "height": 68, "type": "m"},
                        {"url": format!("{}/256-link-x", stub.cdn_url), "width": 604, "height": 317, "type": "x"},
                        {"url": format!("{}/1024-link-z", stub.cdn_url), "width": 1200, "height": 630, "type": "z"},
                    ]}}},
                {"type": "video", "video": {"id": 4, "owner_id": 1, "title": "ролик", "duration": 95,
                    "player": "https://vk.com/video_ext.php?oid=1",
                    "image": [{"url": format!("{}/320-video-l", stub.cdn_url), "width": 320, "height": 180, "type": "l"}]}},
                {"type": "audio_message", "audio_message": {"id": 5, "duration": 7,
                    "link_ogg": format!("{}/777-voice", stub.cdn_url), "waveform": [1, 5, 9]}},
                // Запись на стене: содержимое не разбирается вовсе — ни
                // превью, ни файла, и карточка обязана это честно объявить.
                {"type": "wall", "wall": {"id": 6, "text": "пост"}},
            ],
        }],
        "profiles": [{"id": 1, "first_name": "Павел", "last_name": "Дуров", "screen_name": "durov"}],
        "groups": [],
    })
}

/// РЕГРЕСС ЖИВОГО ДЕФЕКТА: превью ссылки скачивается через наш прокси.
///
/// Симптом на живом аккаунте: карточка ссылки на `bitrix24` показывала
/// значок-заглушку, хотя размеры превью (1200×630) до фронта доезжали.
///
/// Проверяется вся цепочка: `getById` → `attach::url_for` (ветка `KIND_OTHER`)
/// → CDN → кэш → байты клиенту. Плюс честный отказ на `full`: файла у ссылки
/// не бывает, и делать вид, что бывает, нельзя.
#[tokio::test]
async fn a_link_preview_is_served_through_the_proxy() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    let reply = every_kind_message(&stub, 2_000_000_005, 5001);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(reply.clone()), Reply::Response(reply)],
    );

    // Индекс 2 — вложение `link`.
    let (status, headers, body) =
        get_raw(app.clone(), "/api/vk/media/2000000005/5001/2?kind=thumb").await;
    assert_eq!(status, StatusCode::OK, "превью ссылки обязано доехать");
    assert_eq!(
        body.len(),
        256,
        "выбран МИНИМАЛЬНЫЙ размер от 200 px по большей стороне (604×317)"
    );
    assert_eq!(headers[axum::http::header::CONTENT_TYPE], "image/jpeg");
    assert!(
        stub.state.cdn_calls().contains(&"256-link-x".to_string()),
        "скачан именно вариант превью ссылки: {:?}",
        stub.state.cdn_calls()
    );

    let (status, _, body) = get_raw(app, "/api/vk/media/2000000005/5001/2?kind=full").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "файла у ссылки не бывает");
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "no_media");
}

/// ГЕЙТ КЛАССА ДЕФЕКТОВ, СКВОЗНОЙ: чему лента ОБЕЩАЛА файл — то роут медиа
/// обязан ОТДАТЬ, и наоборот.
///
/// Это тот же инвариант, что unit-тест `attach::promises_never_outrun_url_for`,
/// но проверенный через оба реальных метода VK: флаги `has_thumb`/`has_full`
/// приезжают из `messages.getHistory`, а байты — из `messages.getById`. Правка,
/// которая рассинхронизирует эти два пути (или урежет одну из форм ответа),
/// покраснеет здесь, даже если чистые функции по отдельности согласованы.
///
/// «И наоборот» — не украшение: без обратной проверки инвариант удовлетворялся
/// бы тривиально, если бы `has_thumb` объявили `false` для всего подряд.
///
/// МУТАЦИОННАЯ МИШЕНЬ: вернуть `has_thumb = !p.sizes.is_empty()` в ветку
/// `from_link` ⇒ обещание переживёт способность и тест покраснеет.
#[tokio::test]
async fn every_promised_thumb_is_actually_downloadable() {
    let stub = start_stub().await;
    let (_dir, app) = data_app(&stub).await;
    let reply = every_kind_message(&stub, 2_000_000_005, 5001);

    stub.state.script(
        "messages.getHistory",
        vec![Reply::Response(reply.clone())],
    );
    let (status, page) = get_json(
        app.clone(),
        "/api/vk/dialogs/2000000005/messages?limit=50",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let atts = page["messages"][0]["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 6, "все вложения доехали до ленты");

    // Карточка ссылки — ровно то, что видел пользователь.
    let link = &atts[2];
    assert_eq!(link["kind"], "other");
    assert_eq!(link["title"], "Войти в Битрикс24");
    assert_eq!((link["width"].as_i64(), link["height"].as_i64()), (Some(1200), Some(630)));
    assert_eq!(link["has_thumb"], true, "превью ссылки объявлено");
    assert_eq!(
        link["link_url"], "https://psph.example/crm/contact/details/27307/",
        "адрес ссылки доезжает до карточки"
    );

    // Каждое скачивание резолвит сообщение заново — по ответу на брата.
    stub.state.script(
        "messages.getById",
        (0..12).map(|_| Reply::Response(reply.clone())).collect(),
    );

    let mut promised_thumbs = 0;
    let mut promised_fulls = 0;
    for att in atts {
        let idx = att["index"].as_u64().unwrap();
        let kind = att["kind"].as_str().unwrap();
        for (flag, param) in [("has_thumb", "thumb"), ("has_full", "full")] {
            let promised = att[flag].as_bool().unwrap();
            let (status, _, body) = get_raw(
                app.clone(),
                &format!("/api/vk/media/2000000005/5001/{idx}?kind={param}"),
            )
            .await;
            if promised {
                if param == "thumb" {
                    promised_thumbs += 1;
                } else {
                    promised_fulls += 1;
                }
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "вложение {idx} ({kind}) обещало {param}, но роут ответил {status}: {}",
                    String::from_utf8_lossy(&body)
                );
                assert!(!body.is_empty(), "вложение {idx} ({kind}): пустой {param}");
            } else {
                assert_eq!(
                    status,
                    StatusCode::NOT_FOUND,
                    "вложение {idx} ({kind}) НЕ обещало {param}, но роут его отдал"
                );
                let v: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(v["error"]["code"], "no_media");
            }
        }
    }

    // Табличка не выродилась: обещания реально были, и по обеим осям.
    assert_eq!(promised_thumbs, 4, "photo, doc, link, video");
    assert_eq!(promised_fulls, 3, "photo, doc, voice");
}

/// Кадры `download_progress` несут ИНДЕКС вложения: без него прогресс второго
/// фото прилетал бы в карточку первого.
#[tokio::test]
async fn download_progress_frames_carry_the_index() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = forge_vk::state::VkState::new(VkConfig {
        access_token: SecretToken::new(TOKEN),
        base_url: stub.base_url.clone(),
        rate_limit_rps: 50,
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        ..VkConfig::default()
    });
    state
        .set_login(forge_vk::login::LoginFlow::Authorized {
            user: forge_vk::login::VkUserDto {
                id: "999".into(),
                ..Default::default()
            },
        })
        .await;
    let mut rx = state.subscribe();
    let app = forge_vk::routes::build_router(state);

    stub.state.script(
        "messages.getById",
        vec![Reply::Response(gallery_reply_on(&stub, 777, 42, 3))],
    );
    let (status, _, _) = get_raw(app, "/api/vk/media/777/42/2?kind=full").await;
    assert_eq!(status, StatusCode::OK);

    let mut seen = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if let forge_vk::VkServerMsg::DownloadProgress {
            dialog_id,
            message_id,
            index,
            kind,
            received,
            total,
        } = frame
        {
            seen.push((dialog_id, message_id, index, kind, received, total));
        }
    }
    assert!(!seen.is_empty(), "прогресс обязан вещаться");
    for (dialog_id, message_id, index, kind, _, _) in &seen {
        assert_eq!(dialog_id, "777");
        assert_eq!(*message_id, 42);
        assert_eq!(*index, 2, "кадр без индекса попал бы в чужую карточку");
        assert_eq!(kind, "full");
    }
    let last = seen.last().unwrap();
    assert_eq!(last.4, 4096, "финальный кадр — по фактически принятому");
    assert_eq!(last.5, Some(4096));
}

// ===========================================================================
// Отправка файлов (Ф8): staging → три шага VK → messages.send
// ===========================================================================

/// Плагин, авторизованный и настроенный на стенд, ВМЕСТЕ с состоянием.
///
/// Отличается от [`data_app`] только тем, что отдаёт `Arc<VkState>`: тесты
/// загрузки смотрят в реестр `uploads` и подписываются на кадры прогресса, а
/// по одному роутеру этого не увидеть. `max_upload_bytes` урезан — настоящие
/// 2 ГиБ в тесте не пришлёшь.
async fn upload_app(
    stub: &Stub,
    max_upload_bytes: u64,
) -> (
    tempfile::TempDir,
    Arc<forge_vk::state::VkState>,
    axum::Router,
) {
    use forge_vk::login::{LoginFlow, VkUserDto};
    use forge_vk::state::VkState;

    let dir = tempfile::tempdir().unwrap();
    let state = VkState::new(VkConfig {
        access_token: SecretToken::new(TOKEN),
        base_url: stub.base_url.clone(),
        rate_limit_rps: 50,
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        max_upload_bytes,
        ..VkConfig::default()
    });
    state
        .set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "999".into(),
                ..Default::default()
            },
        })
        .await;
    let app = forge_vk::routes::build_router(state.clone());
    (dir, state, app)
}

/// `POST /api/vk/upload` сырым телом — как это делает XHR фронта.
async fn stage_file(app: &axum::Router, name: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let len = bytes.len();
    let resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/vk/upload")
                .header("content-type", "application/octet-stream")
                .header("x-forge-file-name", name)
                .header("content-length", len)
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

/// Кладёт файл в staging и отдаёт `upload_id`.
async fn stage_id(app: &axum::Router, name: &str, size: usize) -> String {
    let (status, json) = stage_file(app, name, vec![b'A'; size]).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json["upload_id"].as_str().unwrap().to_string()
}

/// Скриптует три шага фото-конвейера: адрес, ответ upload-сервера (его отдаёт
/// сам стенд) и сохранение `count` фотографий.
fn script_photo_pipeline(stub: &Stub, count: usize, send_id: i64) {
    stub.state.script(
        "photos.getMessagesUploadServer",
        vec![Reply::Response(json!({"upload_url": stub.upload_url}))],
    );
    let saved: Vec<Value> = (0..count)
        .map(|i| json!({"id": 1000 + i as i64, "owner_id": 42, "access_key": "k"}))
        .collect();
    stub.state
        .script("photos.saveMessagesPhoto", vec![Reply::Response(json!(saved))]);
    stub.state
        .script("messages.send", vec![Reply::Response(json!(send_id))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{"id": send_id, "peer_id": 1234, "date": 1, "from_id": 999, "out": 1, "text": ""}]
        }))],
    );
}

/// Скриптует конвейер документа/голосового.
fn script_doc_pipeline(stub: &Stub, send_id: i64, kind: &str) {
    stub.state.script(
        "docs.getMessagesUploadServer",
        vec![Reply::Response(json!({
            "upload_url": format!("{}/upload/doc", stub.root)
        }))],
    );
    stub.state.script(
        "docs.save",
        vec![Reply::Response(json!({
            "type": kind,
            kind: {"id": 700, "owner_id": 42, "title": "отчёт.pdf"}
        }))],
    );
    stub.state
        .script("messages.send", vec![Reply::Response(json!(send_id))]);
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(json!({
            "count": 1,
            "items": [{"id": send_id, "peer_id": 1234, "date": 1, "from_id": 999, "out": 1, "text": ""}]
        }))],
    );
}

/// `POST /api/vk/upload` НЕ трогает VK — это и есть структурное решение фазы.
///
/// Кадров `upload_progress` на этом шаге тоже нет: они приходят во время
/// `POST /messages`, и фронт обязан продолжать слушать их после «отправить».
#[tokio::test]
async fn staging_touches_neither_vk_nor_the_progress_channel() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 1024 * 1024).await;
    let mut rx = state.subscribe();

    let (status, json) = stage_file(&app, "%D1%84%D0%BE%D1%82%D0%BE.jpg", vec![b'A'; 4096]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["file_name"], "фото.jpg");
    assert_eq!(json["mime"], "image/jpeg");
    assert_eq!(json["size"], 4096);

    assert!(stub.state.calls().is_empty(), "ни одного вызова VK: {:?}", stub.state.calls());
    assert!(
        stub.state.uploads.lock().unwrap().is_empty(),
        "upload-сервер VK на этом шаге даже не выбран"
    );
    assert!(rx.try_recv().is_err(), "кадров прогресса быть не должно");
    // Запись легла в реестр, файл — на диск.
    let id = json["upload_id"].as_str().unwrap();
    let entry = state.upload_entry(id).await.expect("запись в реестре");
    assert_eq!(tokio::fs::metadata(&entry.path).await.unwrap().len(), 4096);
}

/// ОДНО фото = ТРИ троттленных запроса: адрес, сохранение, отправка. Сама
/// заливка идёт мимо троттлера (чужой хост).
#[tokio::test]
async fn a_single_photo_costs_three_throttled_calls() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7100);

    let id = stage_id(&app, "photo.jpg", 4096).await;
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id, "caption": "подпись"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["message"]["id"], 7100);

    // Три ТРОТТЛЕННЫХ вызова конвейера (+ getById за полным DTO — это не часть
    // конвейера, а контракт ответа отправки).
    assert_eq!(stub.state.calls_of("photos.getMessagesUploadServer").len(), 1);
    assert_eq!(stub.state.calls_of("photos.saveMessagesPhoto").len(), 1);
    assert_eq!(stub.state.calls_of("messages.send").len(), 1);
    // И ровно один multipart-POST на upload-сервер.
    assert_eq!(stub.state.uploads.lock().unwrap().len(), 1);

    let server_call = &stub.state.calls_of("photos.getMessagesUploadServer")[0];
    assert_eq!(
        server_call.params.get("peer_id").map(String::as_str),
        Some("1234"),
        "адрес выдаётся ПОД КОНКРЕТНЫЙ peer_id — отсюда и место конвейера"
    );

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(
        send.params.get("attachment").map(String::as_str),
        Some("photo42_1000_k"),
        "attachment-строка рождается на шаге save и живёт до send: {:?}",
        send.params
    );
    assert_eq!(send.params.get("message").map(String::as_str), Some("подпись"));

    // Загрузка потреблена, staging убран.
    assert!(state.upload_entry(&id).await.is_none());
    assert_eq!(
        std::fs::read_dir(dir_of(&state)).map(|rd| rd.count()).unwrap_or(0),
        0,
        "staging-файл после отправки не нужен"
    );
}

/// Каталог staging'а состояния (у теста нет доступа к приватным путям).
fn dir_of(state: &Arc<forge_vk::state::VkState>) -> std::path::PathBuf {
    forge_vk::routes::upload::uploads_dir(&state.config)
}

/// АЛЬБОМ: пять фото стоят ТЕХ ЖЕ трёх троттленных запросов, а не пятнадцати.
///
/// Это и есть причина, по которой `upload_ids` заложен в контракт сразу: при
/// 3 rps пятнадцать запросов — это пять секунд ожидания вместо одной.
#[tokio::test]
async fn an_album_of_five_photos_still_costs_three_throttled_calls() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 5, 7200);

    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(stage_id(&app, &format!("photo{i}.jpg"), 1024).await);
    }

    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_ids": ids, "text": "альбом"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");

    assert_eq!(
        stub.state.calls_of("photos.getMessagesUploadServer").len(),
        1,
        "адрес берётся ОДИН раз на альбом"
    );
    assert_eq!(
        stub.state.calls_of("photos.saveMessagesPhoto").len(),
        1,
        "сохранение — тоже одно на пятёрку"
    );
    assert_eq!(stub.state.calls_of("messages.send").len(), 1);

    // ОДИН multipart-POST с полями file1..file5. Блок нужен ради `await`
    // ниже: `std::sync::MutexGuard` не имеет права переживать точку ожидания.
    {
        let uploads = stub.state.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "пятёрка уезжает одним запросом");
        let body = String::from_utf8_lossy(&uploads[0].body);
        for i in 1..=5 {
            assert!(body.contains(&format!("name=\"file{i}\"")), "поле file{i} потерялось");
        }
        assert!(uploads[0].declared_len.is_some(), "Content-Length обязан быть объявлен");
    }

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(
        send.params.get("attachment").map(String::as_str),
        Some("photo42_1000_k,photo42_1001_k,photo42_1002_k,photo42_1003_k,photo42_1004_k"),
        "порядок вложений — порядок пользователя"
    );

    for id in &ids {
        assert!(state.upload_entry(id).await.is_none(), "{id} обязан быть потреблён");
    }
}

/// Документ и голосовое идут ДРУГОЙ веткой: адрес выдаёт `docs.*`, а тип
/// (`doc` | `audio_message`) задаётся при выдаче адреса, а не при сохранении.
#[tokio::test]
async fn documents_and_voice_take_the_docs_branch() {
    for (name, kind, expected_type) in [
        ("отчёт.pdf", "doc", "doc"),
        ("voice.ogg", "audio_message", "audio_message"),
    ] {
        let stub = start_stub().await;
        let (_dir, _state, app) = upload_app(&stub, 1024 * 1024).await;
        script_doc_pipeline(&stub, 7300, kind);

        let id = stage_id(&app, name, 2048).await;
        let (status, json) = post_json(
            app.clone(),
            "/api/vk/dialogs/1234/messages",
            json!({"upload_id": id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: {json}");

        assert_eq!(stub.state.calls_of("docs.getMessagesUploadServer").len(), 1, "{name}");
        assert_eq!(stub.state.calls_of("docs.save").len(), 1, "{name}");
        assert!(
            stub.state.calls_of("photos.getMessagesUploadServer").is_empty(),
            "{name}: фото-ветка не должна была включиться"
        );

        let server_call = &stub.state.calls_of("docs.getMessagesUploadServer")[0];
        assert_eq!(
            server_call.params.get("type").map(String::as_str),
            Some(expected_type),
            "{name}: тип документа задаётся ДО заливки"
        );
        let send = &stub.state.calls_of("messages.send")[0];
        assert_eq!(
            send.params.get("attachment").map(String::as_str),
            Some("doc42_700"),
            "{name}: у голосового префикс адреса тот же `doc`"
        );
        // Текста нет вовсе — сообщение-вложение законно.
        assert!(!send.params.contains_key("message"), "{name}: {:?}", send.params);
    }
}

/// `upload_id` РАСХОДУЕТСЯ первой отправкой: повторный `POST /messages` с тем
/// же идентификатором получает `409 invalid_state`, а не отправляет файл
/// второй раз. Двойной клик по «Отправить» — вещь куда более частая, чем
/// неудачная заливка.
#[tokio::test]
async fn a_used_upload_id_is_rejected_with_409() {
    let stub = start_stub().await;
    let (_dir, _state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7400);

    let id = stage_id(&app, "photo.jpg", 1024).await;
    let (status, _) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let sends_before = stub.state.calls_of("messages.send").len();
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"]["code"], "invalid_state");
    assert_eq!(
        stub.state.calls_of("messages.send").len(),
        sends_before,
        "повтор не имеет права дойти до VK"
    );
}

/// Кадры `upload_progress` приходят во время `POST /messages` — единственное
/// поведенческое отличие от Telegram, видимое фронту.
#[tokio::test]
async fn upload_progress_frames_arrive_during_the_send() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 4 * 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7500);

    // Файл заметно больше порога throttle (256 КиБ), чтобы кадров было
    // несколько, а не только финальный.
    let id = stage_id(&app, "big.jpg", 900 * 1024).await;
    let mut rx = state.subscribe();
    assert!(rx.try_recv().is_err(), "после staging канал пуст");

    let (status, _) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut frames = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let forge_vk::VkServerMsg::UploadProgress {
            upload_id,
            sent,
            total,
        } = ev
        {
            frames.push((upload_id, sent, total));
        }
    }
    assert!(frames.len() >= 2, "ожидались первый и финальный кадры: {frames:?}");
    assert!(frames.iter().all(|(u, _, _)| *u == id), "чужой upload_id: {frames:?}");
    let last = frames.last().unwrap();
    assert_eq!(last.1, 900 * 1024, "финальный кадр обязан быть полным");
    assert_eq!(last.2, 900 * 1024);
    for (_, sent, total) in &frames {
        assert!(sent <= total, "{sent} > {total}");
    }
}

/// ТРАНСПОРТНЫЙ ПОВТОР `messages.send` НЕ ДУБЛИРУЕТ СООБЩЕНИЕ и НЕ
/// перезаливает файл: `random_id` генерируется один раз на логическую
/// отправку, и VK дедуплицирует по нему.
#[tokio::test]
async fn a_transport_retry_of_a_file_send_reuses_the_random_id() {
    let stub = start_stub().await;
    let (_dir, _state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7600);
    // Первый `messages.send` обрывается на теле ответа, второй проходит.
    stub.state.script(
        "messages.send",
        vec![Reply::Drop, Reply::Response(json!(7600))],
    );

    let id = stage_id(&app, "photo.jpg", 1024).await;
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["message"]["id"], 7600);

    let sends = stub.state.calls_of("messages.send");
    assert_eq!(sends.len(), 2, "ровно один повтор");
    assert_eq!(
        sends[0].params.get("random_id"),
        sends[1].params.get("random_id"),
        "random_id при повторе обязан совпадать — по нему VK и дедуплицирует"
    );
    assert_eq!(
        sends[0].params.get("attachment"),
        sends[1].params.get("attachment"),
        "attachment-строка та же: файл повторно не заливается"
    );
    assert_eq!(
        stub.state.uploads.lock().unwrap().len(),
        1,
        "повтор отправки не имеет права перезаливать файл"
    );
    assert_eq!(
        stub.state.calls_of("photos.getMessagesUploadServer").len(),
        1,
        "и не имеет права снова просить адрес"
    );
}

/// Отказ VK на шаге сохранения: сообщение НЕ уходит, а `upload_id` считается
/// потраченным — цена того, что двойная отправка невозможна по построению.
#[tokio::test]
async fn a_failed_save_does_not_send_anything() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7700);
    stub.state.script(
        "photos.saveMessagesPhoto",
        vec![
            Reply::Error(vk_error(100, "One of the parameters is invalid")),
            Reply::Error(vk_error(100, "One of the parameters is invalid")),
        ],
    );

    let id = stage_id(&app, "photo.jpg", 1024).await;
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(
        stub.state.calls_of("messages.send").is_empty(),
        "отправки быть не должно"
    );
    assert!(state.upload_entry(&id).await.is_none(), "запись потреблена");
    assert_eq!(
        std::fs::read_dir(dir_of(&state)).map(|rd| rd.count()).unwrap_or(0),
        0,
        "staging провалившейся отправки не остаётся мусором"
    );
}

/// Смешанное сообщение: два фото альбомом + документ = 3 запроса на альбом
/// плюс 2 на документ, а строка `attachment` сохраняет ПОРЯДОК ПОЛЬЗОВАТЕЛЯ.
#[tokio::test]
async fn a_mixed_message_keeps_the_user_order() {
    let stub = start_stub().await;
    let (_dir, _state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 2, 7800);
    stub.state.script(
        "docs.getMessagesUploadServer",
        vec![Reply::Response(json!({
            "upload_url": format!("{}/upload/doc", stub.root)
        }))],
    );
    stub.state.script(
        "docs.save",
        vec![Reply::Response(json!({
            "type": "doc",
            "doc": {"id": 700, "owner_id": 42}
        }))],
    );

    let photo1 = stage_id(&app, "a.jpg", 512).await;
    let doc = stage_id(&app, "b.pdf", 512).await;
    let photo2 = stage_id(&app, "c.png", 512).await;

    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_ids": [photo1, doc, photo2]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(
        send.params.get("attachment").map(String::as_str),
        Some("photo42_1000_k,doc42_700,photo42_1001_k"),
        "документ обязан остаться ВТОРЫМ, как его выбрал пользователь"
    );
    // Фотографии уехали одним запросом, документ — своим.
    assert_eq!(stub.state.uploads.lock().unwrap().len(), 2);
}

/// Больше десяти вложений VK не примет — отказ приходит ДО обращения к VK и
/// ДО потребления записей реестра.
#[tokio::test]
async fn too_many_attachments_are_rejected_before_touching_vk() {
    let stub = start_stub().await;
    let (_dir, state, app) = upload_app(&stub, 1024 * 1024).await;

    let mut ids = Vec::new();
    for i in 0..11 {
        ids.push(stage_id(&app, &format!("p{i}.jpg"), 128).await);
    }
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_ids": ids}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"]["code"], "invalid_input");
    assert!(stub.state.calls().is_empty(), "к VK не ходили");
    for id in &ids {
        assert!(
            state.upload_entry(id).await.is_some(),
            "отказ по форме не имеет права тратить загрузки"
        );
    }
}

/// Один и тот же `upload_id`, названный дважды (`upload_id` + `upload_ids`),
/// не превращается в отказ на собственном запросе: дубли схлопываются.
#[tokio::test]
async fn a_duplicated_upload_id_is_deduplicated() {
    let stub = start_stub().await;
    let (_dir, _state, app) = upload_app(&stub, 1024 * 1024).await;
    script_photo_pipeline(&stub, 1, 7900);

    let id = stage_id(&app, "photo.jpg", 256).await;
    let (status, json) = post_json(
        app.clone(),
        "/api/vk/dialogs/1234/messages",
        json!({"upload_id": id, "upload_ids": [id]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(
        send.params.get("attachment").map(String::as_str),
        Some("photo42_1000_k"),
        "вложение одно, а не два"
    );
}

// ---------------------------------------------------------------------------
// Отказ ДО чтения тела — на сыром TCP.
// ---------------------------------------------------------------------------

/// Поднимает роутер на свободном порту и отдаёт адрес.
async fn serve(app: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Отправляет заголовки и НЕПОЛНОЕ тело, ждёт ответ.
async fn raw_upload(addr: std::net::SocketAddr, head: String, body: &[u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.write_all(body).await.unwrap();
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut raw)).await;
    assert!(
        read.is_ok(),
        "ответ не пришёл за 5 с — значит сервер ждал тело целиком"
    );
    String::from_utf8_lossy(&raw).into_owned()
}

/// ОТКАЗ ДО ЧТЕНИЯ ТЕЛА.
///
/// Заголовок обещает 64 МиБ, реально уходит десяток байт. Если бы хендлер
/// читал тело до проверок, ответа не было бы вовсе и тест упал бы по таймауту.
/// Проверяется тот самый порядок из шапки `routes/upload.rs`: авторизация →
/// `Content-Length` → лимит → имя файла → и только потом стриминг.
#[tokio::test]
async fn upload_refuses_before_reading_the_body() {
    let stub = start_stub().await;
    // Лимит урезан до килобайта: 64 МиБ заведомо сверх него.
    let (_dir, _state, app) = upload_app(&stub, 1024).await;
    let addr = serve(app).await;

    // (1) Сверх лимита — 413, тело не прочитано.
    let text = raw_upload(
        addr,
        format!(
            "POST /api/vk/upload HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/octet-stream\r\n\
             X-Forge-File-Name: big.bin\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            64 * 1024 * 1024
        ),
        b"0123456789",
    )
    .await;
    assert!(text.starts_with("HTTP/1.1 413"), "ожидался 413, получено: {text}");
    assert!(text.contains("\"too_large\""), "{text}");

    // (2) Без Content-Length — 411, и тоже без ожидания тела.
    let text = raw_upload(
        addr,
        format!(
            "POST /api/vk/upload HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/octet-stream\r\n\
             X-Forge-File-Name: big.bin\r\n\
             Connection: close\r\n\r\n"
        ),
        b"",
    )
    .await;
    assert!(text.starts_with("HTTP/1.1 411"), "ожидался 411, получено: {text}");
    assert!(text.contains("\"length_required\""), "{text}");

    // Ни одного обращения к VK ни в одном из отказов.
    assert!(stub.state.calls().is_empty());
}

/// Без авторизации отказ тоже приходит ДО чтения тела — иначе клиент зря
/// заливал бы гигабайт, чтобы узнать, что аккаунт не привязан.
#[tokio::test]
async fn upload_without_auth_refuses_before_reading_the_body() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    // Состояние БЕЗ логина.
    let state = forge_vk::state::VkState::new(VkConfig {
        base_url: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        ..VkConfig::default()
    });
    let addr = serve(forge_vk::routes::build_router(state)).await;

    let text = raw_upload(
        addr,
        format!(
            "POST /api/vk/upload HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/octet-stream\r\n\
             X-Forge-File-Name: big.bin\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n",
            64 * 1024 * 1024
        ),
        b"0123456789",
    )
    .await;
    assert!(text.starts_with("HTTP/1.1 409"), "ожидался 409, получено: {text}");
    assert!(text.contains("\"not_authorized\""), "{text}");
}

// ===========================================================================
// Клавиатуры сообществ (Ф9): нажатие по координатам и честная деградация
//
// Главное, что проверяется здесь, — ЧЕГО НЕ ПРОИСХОДИТ. Кнопка вида
// `callback` не превращается ни в какой запрос к VK: клиентской половины
// `message_event` в публичном API нет вовсе, и «нажать» её нечем. Такая
// кнопка обязана отвечать ровно одним способом — `409 unsupported_button` с
// объяснением, а не молчанием, не «ошибкой сервера» и не имитацией нажатия.
// ===========================================================================

/// Ответ `messages.getById` с сообществом, приславшим клавиатуру.
///
/// Первый ряд — `text` (нажимаемая) и `open_link`, второй — `callback`.
/// `payload` у каждой свой: тест нажатия проверяет, что уехал именно тот.
fn keyboard_message(message_id: i64, peer_id: i64) -> Value {
    json!({
        "count": 1,
        "items": [{
            "id": message_id, "peer_id": peer_id, "date": 1_700_000_700_i64,
            "from_id": peer_id, "text": "Выберите действие",
            "conversation_message_id": 800,
            "keyboard": {
                "one_time": false,
                "inline": true,
                "author_id": peer_id,
                "buttons": [
                    [
                        {"color": "primary", "action": {
                            "type": "text", "label": "Показать заказы",
                            "payload": "{\"cmd\":\"orders\"}"
                        }},
                        {"color": "secondary", "action": {
                            "type": "open_link", "label": "Сайт",
                            "link": "https://example.com/shop",
                            "payload": "{\"cmd\":\"site\"}"
                        }}
                    ],
                    [
                        {"color": "negative", "action": {
                            "type": "callback", "label": "Отменить",
                            "payload": "{\"cmd\":\"cancel\"}"
                        }}
                    ]
                ]
            }
        }],
        "groups": [{"id": -peer_id, "name": "Магазин", "screen_name": "shop"}]
    })
}

/// Клавиатура доезжает до клиента с флагами поддержки и БЕЗ `payload`.
///
/// Мутация «отдать payload наружу» краснеет здесь: фронт получил бы всё нужное
/// для подделки команды сообществу.
#[tokio::test]
async fn the_keyboard_reaches_the_client_without_any_payload() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(keyboard_message(7100, -70))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = get_json(app, "/api/vk/dialogs/-70/messages/7100").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let kb = &json["message"]["reply_markup"];
    assert_eq!(kb["kind"], "inline");
    assert_eq!(kb["single_use"], false);

    let first = kb["rows"][0].as_array().unwrap();
    assert_eq!(first[0]["kind"], "text");
    assert_eq!(first[0]["supported"], true);
    assert_eq!(first[0]["text"], "Показать заказы");
    assert_eq!((first[0]["row"].as_i64(), first[0]["col"].as_i64()), (Some(0), Some(0)));
    assert_eq!(first[1]["kind"], "open_link");
    assert_eq!(first[1]["supported"], true);
    assert_eq!(first[1]["url"], "https://example.com/shop");

    let second = kb["rows"][1].as_array().unwrap();
    assert_eq!(second[0]["kind"], "callback");
    assert_eq!(
        second[0]["supported"], false,
        "callback под пользовательским токеном нажать нельзя"
    );
    assert_eq!((second[0]["row"].as_i64(), second[0]["col"].as_i64()), (Some(1), Some(0)));

    let body = json.to_string();
    assert!(!body.contains("cmd"), "payload кнопки уехал во фронт: {body}");
    assert!(!body.contains("orders"), "payload кнопки уехал во фронт: {body}");
}

/// Нажатие `text`-кнопки — это обычная отправка ЕЁ ПОДПИСИ вместе с `payload`.
///
/// Именно так работает официальный клиент VK, и именно поэтому «следование»
/// ленты за ответом сообщества (`js/messenger/follow-core.js`) работает без
/// единой особой ветки.
#[tokio::test]
async fn pressing_a_text_button_sends_its_label_and_payload() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![
            // 1) читаем клавиатуру, 2) добираем отправленное сообщение.
            Reply::Response(keyboard_message(7100, -70)),
            Reply::Response(json!({
                "count": 1,
                "items": [{
                    "id": 7101, "peer_id": -70, "date": 1_700_000_710_i64, "from_id": 999,
                    "out": 1, "text": "Показать заказы", "conversation_message_id": 801
                }],
                "profiles": [{"id": 999, "first_name": "Я", "last_name": "Сам"}]
            })),
        ],
    );
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7101))]);
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 0, "col": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["message"]["id"], 7101);
    assert_eq!(json["message"]["out"], true);
    assert_eq!(json["message"]["text"], "Показать заказы");

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(send.params.get("peer_id").unwrap(), "-70");
    assert_eq!(
        send.params.get("message").unwrap(),
        "Показать заказы",
        "боту уходит ПОДПИСЬ кнопки"
    );
    assert_eq!(
        send.params.get("payload").unwrap(),
        "{\"cmd\":\"orders\"}",
        "payload взят из сообщения, а не из тела запроса"
    );
    let random_id: i64 = send.params.get("random_id").unwrap().parse().unwrap();
    assert!(random_id > 0, "random_id=0 у VK означает «не дедуплицировать»");
}

/// ГЛАВНАЯ ПРОВЕРКА ФАЗЫ: `callback` отвергается честно и НЕ порождает
/// никакого запроса к VK.
///
/// Мутация «считать callback нажимаемым» краснеет здесь дважды: статусом и
/// появившимся вызовом `messages.send`.
#[tokio::test]
async fn pressing_a_callback_button_is_refused_with_an_explanation() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(keyboard_message(7100, -70))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 1, "col": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"]["code"], "unsupported_button");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("ВКонтакте не позволяет нажать кнопку этого типа"),
        "объяснение обязано быть человеческим: {message}"
    );
    assert!(message.contains("callback"), "{message}");
    assert!(
        stub.state.calls_of("messages.send").is_empty(),
        "нажатия не было — отправлять нечего"
    );

    // `open_link` сервер тоже не «нажимает»: её открывает браузер клиента.
    let (_dir, app) = data_app(&stub).await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(keyboard_message(7100, -70))],
    );
    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 0, "col": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"]["code"], "unsupported_button");
    assert!(
        stub.state.calls_of("messages.send").is_empty(),
        "ссылку открывает клиент, а не сервер"
    );
}

/// Координаты вне матрицы — отказ, а не «отправка чего попало».
///
/// Второй сценарий важнее первого: тело БЕЗ координат обязано отвергаться до
/// единого обращения к VK, иначе «сломанный клиент» стоил бы запроса из
/// бюджета 3 rps.
#[tokio::test]
async fn coordinates_outside_the_keyboard_send_nothing() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![Reply::Response(keyboard_message(7100, -70))],
    );
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 5, "col": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"]["code"], "invalid_input");
    assert!(stub.state.calls_of("messages.send").is_empty());

    // Тела нет вовсе / координат нет — отказ ДО обращения к VK.
    for body in [json!({}), json!({"row": 0}), json!({"row": -1, "col": 0})] {
        let stub2 = start_stub().await;
        let (_dir, app) = data_app(&stub2).await;
        let (status, json) = post_json(
            app,
            "/api/vk/dialogs/-70/messages/7100/button",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {json}");
        assert_eq!(json["error"]["code"], "invalid_input", "{body}");
        assert!(
            stub2.state.calls().is_empty(),
            "{body}: разбор координат обязан случиться до вызова VK"
        );
    }
}

/// PAYLOAD С ФРОНТА ПОДДЕЛАТЬ НЕЧЕМ.
///
/// Мутация «принимать payload из тела запроса» краснеет здесь: тело несёт и
/// `payload`, и `message`, и `label`, а боту всё равно уезжает то, что лежало
/// в САМОМ сообщении.
#[tokio::test]
async fn a_payload_in_the_request_body_is_ignored() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![
            Reply::Response(keyboard_message(7100, -70)),
            Reply::Response(json!({
                "count": 1,
                "items": [{
                    "id": 7102, "peer_id": -70, "date": 1_700_000_720_i64, "from_id": 999,
                    "out": 1, "text": "Показать заказы", "conversation_message_id": 802
                }]
            })),
        ],
    );
    stub.state
        .script("messages.send", vec![Reply::Response(json!(7102))]);
    let (_dir, app) = data_app(&stub).await;

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({
            "row": 0, "col": 0,
            "payload": "{\"cmd\":\"admin_drop_everything\"}",
            "message": "перевести 1000 рублей",
            "label": "перевести 1000 рублей",
            "text": "перевести 1000 рублей"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");

    let send = &stub.state.calls_of("messages.send")[0];
    assert_eq!(send.params.get("payload").unwrap(), "{\"cmd\":\"orders\"}");
    assert_eq!(send.params.get("message").unwrap(), "Показать заказы");
    let raw = format!("{:?}", send.params);
    assert!(!raw.contains("admin_drop_everything"), "{raw}");
    assert!(!raw.contains("1000"), "{raw}");
}

/// Клавиатура успела измениться (бот переписал сообщение) — честный отказ.
///
/// Сообщения нет вовсе ⇒ `404 unknown_message`; сообщение есть, а кнопки в нём
/// уже нет ⇒ `400 invalid_input`. Ни в одном случае никто ничего не отправляет.
#[tokio::test]
async fn a_stale_keyboard_never_turns_into_a_random_message() {
    let stub = start_stub().await;
    stub.state.script(
        "messages.getById",
        vec![
            // Сообщение из ДРУГОЙ беседы (подстановка чужого peer в путь).
            Reply::Response(json!({
                "count": 1,
                "items": [{"id": 7100, "peer_id": 4321, "date": 1, "text": "чужое"}]
            })),
            // Сообщение без клавиатуры вовсе.
            Reply::Response(json!({
                "count": 1,
                "items": [{"id": 7100, "peer_id": -70, "date": 1, "text": "кнопок больше нет"}]
            })),
        ],
    );

    let (_dir, app) = data_app(&stub).await;
    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 0, "col": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    assert_eq!(json["error"]["code"], "unknown_message");

    let (_dir, app) = data_app(&stub).await;
    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/-70/messages/7100/button",
        json!({"row": 0, "col": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"]["code"], "invalid_input");
    assert!(stub.state.calls_of("messages.send").is_empty());
}

/// Лестница проверок соблюдена и для нажатия: без авторизации роут отвечает
/// `409`, не разбирая ни путь, ни тело (иначе он стал бы оракулом валидности
/// идентификаторов).
#[tokio::test]
async fn pressing_a_button_without_a_token_hits_authorization_first() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = forge_vk::state::VkState::new(VkConfig {
        base_url: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        config_path: dir.path().join("vk.toml"),
        ..VkConfig::default()
    });
    let app = forge_vk::routes::build_router(state);

    let (status, json) = post_json(
        app,
        "/api/vk/dialogs/abc/messages/0/button",
        json!({"row": 999999, "col": -5}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"]["code"], "not_authorized");
    assert!(stub.state.calls().is_empty());
}
