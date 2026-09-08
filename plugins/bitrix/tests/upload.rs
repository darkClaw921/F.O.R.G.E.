//! Интеграционные тесты ОТПРАВКИ ФАЙЛОВ (Ф9): лестница приёма, staging,
//! одноразовость `upload_id`, отсутствие ретрая, двухфазный прогресс.
//!
//! # Свой стенд, а не `b24_stub.rs`
//!
//! Стенд транспорта (`tests/b24_stub.rs`) отвечает готовыми фикстурами; здесь
//! же нужен ЖУРНАЛ ТЕЛ ЗАПРОСОВ — именно им доказывается, что в портал уехал
//! правильный base64, что `im.v2.File.upload` был вызван РОВНО ОДИН РАЗ и что
//! `im.message.add` следом не вызывался вовсе.
//!
//! Порт всегда `127.0.0.1:0`: жёсткий однажды упёрся бы в живой devforge на
//! 7331 и сломал бы прогон на машине разработчика.
//!
//! # Что здесь проверяется
//!
//! | тест | инвариант фазы |
//! |---|---|
//! | `a_file_travels_from_staging_to_the_portal_in_one_call` | Р7: одношаговая заливка, base64 из файла, ОДИН вызов |
//! | `the_upload_never_sends_a_second_message` | `im.message.add` после заливки НЕ зовётся |
//! | `an_empty_caption_is_not_sent_as_a_message_field` | пустая подпись не превращается в `MESSAGE_EMPTY` |
//! | `a_transport_failure_costs_exactly_one_portal_call` | Р9: заливка не ретраится |
//! | `a_consumed_upload_id_cannot_be_used_twice` | защита от дубля на уровне механики |
//! | `a_failed_upload_still_discards_the_staging_file` | полуфабрикат не переживает попытку |
//! | `the_portal_phase_is_indeterminate_and_always_finishes` | Р8: финал приходит и при ошибке |
//! | `the_ladder_rejects_before_reading_the_body` | 413 выдаётся ДО чтения тела |
//! | `an_unknown_upload_id_is_a_409_without_touching_the_portal` | чужой идентификатор не стоит вызова портала |

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::progress::{PHASE_CLIENT, PHASE_PORTAL};
use forge_bitrix::routes::upload::{staging_path, uploads_dir, FILE_NAME_HEADER};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow};
use forge_bitrix::ws::protocol::B24ServerMsg;

/// Код вебхука, который стенд обязан увидеть в пути REST-вызова.
const CODE: &str = "wqdzbfhmn83d23j4";

/// Пользователь вебхука.
const UID: &str = "1";

/// Диалог-полигон (тот же, что на живом портале разведки).
const DIALOG: &str = "chat27";

/// Идентификатор сообщения, который стенд возвращает на заливку.
const MID: i64 = 97;

// ===========================================================================
// Стенд портала.
// ===========================================================================

#[derive(Default)]
struct StubState {
    /// Имена вызванных методов по порядку.
    methods: Mutex<Vec<String>>,
    /// Тела вызовов `im.v2.File.upload`.
    uploads: Mutex<Vec<Value>>,
    /// Сколько раз стенд обрывал ответ (сценарий транспортной ошибки).
    drops: AtomicUsize,
    /// Обрывать ли соединение вместо ответа на заливку.
    fail_transport: Mutex<bool>,
    /// Отвечать ли на заливку ошибкой портала (код).
    fail_api: Mutex<Option<String>>,
}

impl StubState {
    fn method_count(&self, name: &str) -> usize {
        self.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == name)
            .count()
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }

    fn last_upload(&self) -> Value {
        self.uploads
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or(Value::Null)
    }
}

struct Stub {
    base_url: String,
    state: Arc<StubState>,
}

async fn start_stub() -> Stub {
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .route("/rest/:uid/:code/:method", post(handle_method))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Stub {
        base_url: format!("http://{addr}/rest/{UID}/{CODE}"),
        state,
    }
}

fn time_block() -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "start": now, "finish": now as f64 + 0.1, "duration": 0.1, "processing": 0,
        "date_start": "2026-08-06T10:00:00+03:00", "date_finish": "2026-08-06T10:00:01+03:00",
        "operating_reset_at": now + 600, "operating": 0.0
    })
}

fn ok(result: Value) -> Response {
    axum::Json(json!({"result": result, "time": time_block()})).into_response()
}

fn api_error(code: &str) -> Response {
    axum::Json(json!({"error": code, "error_description": format!("stub: {code}")})).into_response()
}

async fn handle_method(
    State(state): State<Arc<StubState>>,
    Path((_uid, code, method_json)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Response {
    assert_eq!(code, CODE, "клиент собрал URL не с тем кодом вебхука");
    let method = method_json
        .strip_suffix(".json")
        .unwrap_or(&method_json)
        .to_string();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.methods.lock().unwrap().push(method.clone());

    match method.as_str() {
        "im.v2.File.upload" => {
            state.uploads.lock().unwrap().push(parsed);
            if *state.fail_transport.lock().unwrap() {
                state.drops.fetch_add(1, Ordering::SeqCst);
                // Пустое тело при HTTP 200 — ровно то, чем живой портал
                // отвечает на превышение интенсивности; клиент обязан
                // трактовать это как транспортный отказ.
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap();
            }
            if let Some(err) = state.fail_api.lock().unwrap().clone() {
                return api_error(&err);
            }
            ok(json!({
                "file": {"id": 95, "name": "probe.txt", "type": "file", "size": 20},
                "messageId": MID,
                "chatId": 27,
                "dialogId": DIALOG
            }))
        }
        // Обычная текстовая отправка: нужна ровно одному тесту — тому, что
        // проверяет, что пустой `upload_id` ведёт НЕ в файловую ветку.
        "im.message.add" => ok(json!(MID)),
        // Перечитывание строки после отправки.
        "im.dialog.messages.get" => ok(json!({
            "chat_id": 27,
            "messages": [{
                "id": MID, "chat_id": 27, "author_id": 1,
                "date": "2026-08-06T10:00:01+03:00",
                "text": "", "unread": false, "params": {"FILE_ID": [95]}
            }],
            "users": [],
            "files": [{
                "id": 95, "chatId": 27, "name": "probe.txt", "extension": "txt",
                "size": 20, "type": "file", "image": false, "status": "done", "authorId": 1
            }]
        })),
        "im.user.list.get" => ok(json!({})),
        _ => ok(json!(true)),
    }
}

// ===========================================================================
// Обвязка.
// ===========================================================================

struct Harness {
    _dir: tempfile::TempDir,
    stub: Arc<StubState>,
    state: Arc<B24State>,
    app: Router,
}

async fn harness() -> Harness {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        webhook: WebhookUrl::new(format!("http://example.invalid/rest/{UID}/{CODE}/")),
        base_url_override: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        ..BitrixConfig::default()
    };
    let state = B24State::new(cfg);
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "stub".into(),
        })
        .await;
    let app = forge_bitrix::routes::build_router(forge_bitrix::B24Registry::single(state.clone()));
    Harness {
        _dir: dir,
        stub: stub.state,
        state,
        app,
    }
}

/// `POST /api/bitrix/upload` с телом `payload`.
async fn post_upload(app: &Router, name: &str, payload: &[u8]) -> Response {
    let req = Request::builder()
        .method("POST")
        .uri("/api/bitrix/upload")
        .header("content-type", "application/octet-stream")
        .header(FILE_NAME_HEADER, name)
        .header("content-length", payload.len())
        .body(Body::from(payload.to_vec()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

/// `POST /api/bitrix/dialogs/:id/messages` с `upload_id`.
async fn post_file_message(app: &Router, upload_id: &str, text: &str) -> Response {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/bitrix/dialogs/{DIALOG}/messages"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"upload_id": upload_id, "text": text}).to_string(),
        ))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Приём файла + возврат `upload_id`.
async fn stage(app: &Router, name: &str, payload: &[u8]) -> String {
    let resp = post_upload(app, name, payload).await;
    assert_eq!(resp.status(), StatusCode::OK, "приём файла обязан пройти");
    body_json(resp).await["upload_id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// ГЛАВНЫЙ ИНВАРИАНТ ФАЗЫ (Р7): файл уезжает в портал ОДНИМ вызовом
/// `im.v2.File.upload`, в теле — base64 ровно тех байтов, что приняли, а имя
/// и подпись едут внутри `fields`.
#[tokio::test]
async fn a_file_travels_from_staging_to_the_portal_in_one_call() {
    let h = harness().await;
    let payload = b"FORGE probe payload\n".to_vec();

    let upload_id = stage(&h.app, "probe.txt", &payload).await;
    let staged = staging_path(&h.state.config, &upload_id);
    assert!(staged.is_file(), "staging-файл обязан лежать на диске");

    let resp = post_file_message(&h.app, &upload_id, "подпись").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["id"], MID, "ответ несёт messageId портала");

    assert_eq!(
        h.stub.method_count("im.v2.File.upload"),
        1,
        "заливка обязана быть ровно одна: {:?}",
        h.stub.methods()
    );
    let sent = h.stub.last_upload();
    assert_eq!(sent["dialogId"], DIALOG);
    assert_eq!(sent["fields"]["name"], "probe.txt");
    assert_eq!(sent["fields"]["message"], "подпись");
    assert_eq!(
        sent["fields"]["content"].as_str().unwrap(),
        base64::engine::general_purpose::STANDARD.encode(&payload),
        "в теле обязан быть base64 ровно принятых байтов"
    );

    // Полуфабрикат израсходован: ни файла, ни записи в реестре.
    assert!(!staged.exists(), "staging-файл обязан исчезнуть после отправки");
    assert!(h.state.uploads.lock().await.is_empty());
}

/// `im.v2.File.upload` уже ОТПРАВЛЯЕТ сообщение — второй вызов `im.message.add`
/// дал бы вторую строку в чате.
#[tokio::test]
async fn the_upload_never_sends_a_second_message() {
    let h = harness().await;
    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    let resp = post_file_message(&h.app, &upload_id, "подпись").await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        h.stub.method_count("im.message.add"),
        0,
        "после заливки НЕЛЬЗЯ звать im.message.add: {:?}",
        h.stub.methods()
    );
}

/// Пустая подпись НЕ уезжает полем `message`: у части методов портала пустой
/// текст — это ошибка параметров, а файл без подписи совершенно нормален.
#[tokio::test]
async fn an_empty_caption_is_not_sent_as_a_message_field() {
    let h = harness().await;
    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    let resp = post_file_message(&h.app, &upload_id, "").await;
    assert_eq!(resp.status(), StatusCode::OK, "файл без подписи — норма");

    let sent = h.stub.last_upload();
    assert!(
        sent["fields"].get("message").is_none(),
        "пустая подпись не должна попадать в запрос: {sent}"
    );
}

/// Р9: транспортный отказ стоит РОВНО ОДНОГО вызова портала. Повтор создал бы
/// второй файл на Диске и вторую строку в чате.
#[tokio::test]
async fn a_transport_failure_costs_exactly_one_portal_call() {
    let h = harness().await;
    *h.stub.fail_transport.lock().unwrap() = true;

    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    let staged = staging_path(&h.state.config, &upload_id);

    let resp = post_file_message(&h.app, &upload_id, "подпись").await;
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "транспортный отказ обязан приехать 503, а не 502"
    );
    assert_eq!(body_json(resp).await["error"]["code"], "not_connected");

    assert_eq!(
        h.stub.method_count("im.v2.File.upload"),
        1,
        "заливка НЕ ретраится (Р9): {:?}",
        h.stub.methods()
    );
    assert!(!staged.exists(), "неудачная попытка не оставляет staging");
}

/// Ошибка портала по существу тоже не ретраится и приезжает по таблице §4.
#[tokio::test]
async fn a_portal_error_is_not_retried_either() {
    let h = harness().await;
    *h.stub.fail_api.lock().unwrap() = Some("ERROR_NO_ACCESS".to_string());

    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    let resp = post_file_message(&h.app, &upload_id, "").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["error"]["code"], "send_forbidden");
    assert_eq!(h.stub.method_count("im.v2.File.upload"), 1);
}

/// `upload_id` ОДНОРАЗОВЫЙ: второй запрос с ним — 409, а не второй файл.
///
/// Двойной клик по «отправить» и автоповтор фронта случаются куда чаще, чем
/// неудачная заливка, и цена ошибки у них разная: лишний файл у собеседника не
/// отзывается.
#[tokio::test]
async fn a_consumed_upload_id_cannot_be_used_twice() {
    let h = harness().await;
    let upload_id = stage(&h.app, "probe.txt", b"data").await;

    let first = post_file_message(&h.app, &upload_id, "").await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = post_file_message(&h.app, &upload_id, "").await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let json = body_json(second).await;
    assert_eq!(json["error"]["code"], "invalid_state");
    assert!(
        !json["error"]["message"].as_str().unwrap().contains(&upload_id),
        "идентификатор не должен отражаться в тексте ошибки: {json}"
    );

    assert_eq!(
        h.stub.method_count("im.v2.File.upload"),
        1,
        "второй запрос не имеет права дойти до портала"
    );
}

/// Неизвестный `upload_id` — это 409 БЕЗ единого вызова портала: чужая строка
/// не должна стоить запроса из бюджета 2 rps.
#[tokio::test]
async fn an_unknown_upload_id_is_a_409_without_touching_the_portal() {
    let h = harness().await;
    let resp = post_file_message(&h.app, "нет-такого", "").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["error"]["code"], "invalid_state");
    assert_eq!(
        h.stub.method_count("im.v2.File.upload"),
        0,
        "портал не должен узнать о попытке: {:?}",
        h.stub.methods()
    );
}

/// Неудачная отправка не оставляет полуфабриката: повторять его запрещено
/// (Р9), а место он занимает.
#[tokio::test]
async fn a_failed_upload_still_discards_the_staging_file() {
    let h = harness().await;
    *h.stub.fail_api.lock().unwrap() = Some("UPLOAD_FAILED".to_string());

    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    let staged = staging_path(&h.state.config, &upload_id);
    assert!(staged.is_file());

    let resp = post_file_message(&h.app, &upload_id, "").await;
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
    assert!(!staged.exists(), "staging обязан быть убран в любом исходе");
    assert!(h.state.uploads.lock().await.is_empty());

    // Каталог при этом остаётся — в него ляжет следующая загрузка.
    assert!(uploads_dir(&h.state.config).is_dir());
}

/// Р8: фаза `portal` индетерминированна, а ФИНАЛЬНЫЙ кадр приходит В ЛЮБОМ
/// исходе — иначе полоса живёт до перезагрузки вкладки.
#[tokio::test]
async fn the_portal_phase_is_indeterminate_and_always_finishes() {
    let h = harness().await;
    *h.stub.fail_api.lock().unwrap() = Some("UPLOAD_FAILED".to_string());

    let upload_id = stage(&h.app, "probe.txt", b"data").await;
    // Подписываемся ПОСЛЕ приёма: кадры фазы client уже прошли, и в выборке
    // останется ровно то, что относится к заливке.
    let mut rx = h.state.subscribe();

    let resp = post_file_message(&h.app, &upload_id, "").await;
    assert!(resp.status().is_client_error() || resp.status().is_server_error());

    let mut portal_frames = 0;
    let mut saw_client = false;
    while let Ok(frame) = rx.try_recv() {
        if let B24ServerMsg::UploadProgress {
            upload_id: id,
            phase,
            indeterminate,
            total,
            ..
        } = frame
        {
            assert_eq!(id, upload_id, "кадр адресуется идентификатором загрузки");
            if phase == PHASE_CLIENT {
                saw_client = true;
            }
            if phase == PHASE_PORTAL {
                portal_frames += 1;
                assert!(
                    indeterminate,
                    "у одношаговой base64-заливки процентов не существует (Р7/Р8)"
                );
                assert_eq!(total, 4);
            }
        }
    }
    assert!(!saw_client, "фаза приёма закончилась до подписки");
    assert!(
        portal_frames >= 2,
        "обязаны прийти начало фазы и ФИНАЛ даже при ошибке, пришло {portal_frames}"
    );
}

/// Лестница приёма: 413 выдаётся ДО чтения тела.
///
/// Здесь это видно по побочному эффекту: каталог staging'а не создан, а реестр
/// пуст — то есть до записи на диск дело не дошло вовсе.
#[tokio::test]
async fn the_ladder_rejects_before_reading_the_body() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = B24State::new(BitrixConfig {
        webhook: WebhookUrl::new(format!("http://example.invalid/rest/{UID}/{CODE}/")),
        base_url_override: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        max_upload_bytes: 8,
        ..BitrixConfig::default()
    });
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "stub".into(),
        })
        .await;
    let app = forge_bitrix::routes::build_router(forge_bitrix::B24Registry::single(state.clone()));

    let resp = post_upload(&app, "big.bin", &[0u8; 64]).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body_json(resp).await["error"]["code"], "too_large");
    assert!(
        !uploads_dir(&state.config).exists(),
        "до создания каталога staging'а дело не должно дойти"
    );
    assert!(state.uploads.lock().await.is_empty());
    assert_eq!(
        stub.state.methods().len(),
        0,
        "отказ по размеру не стоит ни одного вызова портала"
    );
}

/// Без подключения приём файла невозможен — и это первая ступень лестницы, а
/// не следствие чего-то ещё.
#[tokio::test]
async fn an_unauthorized_upload_is_rejected_first() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = B24State::new(BitrixConfig {
        base_url_override: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        ..BitrixConfig::default()
    });
    let app = forge_bitrix::routes::build_router(forge_bitrix::B24Registry::single(state.clone()));

    let resp = post_upload(&app, "a.txt", b"hello").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
    assert!(!uploads_dir(&state.config).exists());
}

/// Юникодное имя доезжает до портала целиком: заголовок percent-encoded, а
/// `fields.name` — уже декодированное имя.
#[tokio::test]
async fn a_unicode_file_name_survives_the_round_trip() {
    let h = harness().await;
    // encodeURIComponent('отчёт за май.pdf')
    let encoded = "%D0%BE%D1%82%D1%87%D1%91%D1%82%20%D0%B7%D0%B0%20%D0%BC%D0%B0%D0%B9.pdf";
    let upload_id = stage(&h.app, encoded, b"pdf-bytes").await;

    let resp = post_file_message(&h.app, &upload_id, "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(h.stub.last_upload()["fields"]["name"], "отчёт за май.pdf");
}

/// Отправка ТЕКСТА не должна ничего знать о загрузках: пустой `upload_id` —
/// это обычное текстовое сообщение, а не «файл без файла».
#[tokio::test]
async fn an_empty_upload_id_falls_back_to_a_plain_text_message() {
    let h = harness().await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/bitrix/dialogs/{DIALOG}/messages"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"upload_id": "   ", "text": "просто текст"}).to_string(),
        ))
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(h.stub.method_count("im.v2.File.upload"), 0);
    assert_eq!(
        h.stub.method_count("im.message.add"),
        1,
        "пустой upload_id обязан вести в обычную отправку: {:?}",
        h.stub.methods()
    );
}
