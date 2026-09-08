//! ТЕСТ-СТРАЖ ИЗОЛЯЦИИ ПОРТАЛОВ на уровне HTTP.
//!
//! # Что охраняется
//!
//! Запрос, адресованный порталу **A**, не имеет права увидеть ни строчки
//! данных портала **B** — ни в списке диалогов, ни в ленте сообщений, ни в
//! списке задач, ни в локальной заметке анализа.
//!
//! # Почему совпадающие идентификаторы — не искусственный случай
//!
//! Идентификаторы у разных порталов НЕЗАВИСИМЫ: `chat7` есть и там, и там, а
//! `MESSAGE_ID` растёт от единицы у каждого портала свой. Поэтому оба стенда
//! здесь отдают ОДИН И ТОТ ЖЕ `dialog_id` и ОДИН И ТОТ ЖЕ `message_id` —
//! различается только СОДЕРЖИМОЕ. Разведи их идентификаторами — и тест
//! проходил бы даже при перепутанном состоянии: строка из чужого портала
//! просто не нашлась бы.
//!
//! # Что именно ломается, если тест упадёт
//!
//! Три разные поломки, и все три молчаливые:
//!
//! 1. **Экстрактор [`forge_bitrix::routes::Portal`] вернул не тот слот** —
//!    панель показывает чужую переписку под своим именем вкладки.
//! 2. **Роутер снова привязан к одному `Arc<B24State>`** (`with_state`
//!    вместо реестра) — заголовок `X-Forge-Portal` начинает молча
//!    игнорироваться, и ВСЕ вкладки показывают первый портал.
//! 3. **Фолбэк стал недетерминированным** (например, «первый ПОДКЛЮЧЁННЫЙ»
//!    вместо «первый слот») — запрос без заголовка начинает гулять между
//!    порталами в зависимости от порядка подключения.
//!
//! # Герметичность
//!
//! Сети наружу нет: каждый портал изображает свой стенд на
//! `127.0.0.1:0` (порт выдаёт ОС), а плагин смотрит на него через
//! `base_url_override` — ровно тот механизм, ради которого это поле и
//! существует (Р17 разрешает `http://` на петлевой адрес). Ни `~/.forge`, ни
//! `~/.config/forge` не трогаются: все пути конфига уводятся в tempdir.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::login::{B24UserDto, LoginFlow};
use forge_bitrix::state::B24State;
use forge_bitrix::B24Registry;

/// Код вебхука в пути стенда. Один на оба портала намеренно: различать их
/// обязан реестр, а не случайное расхождение секретов.
const CODE: &str = "isoc0de1234567890";

/// Диалог, который есть у ОБОИХ порталов.
const DIALOG: &str = "chat7";

/// Сообщение, которое есть у ОБОИХ порталов под одним номером.
const MESSAGE_ID: i64 = 101;

/// Задача, которая есть у ОБОИХ порталов под одним номером.
const TASK_ID: i64 = 42;

// ===========================================================================
// Стенд одного портала.
// ===========================================================================

/// Состояние стенда: «метка» портала, которой помечено ВСЁ его содержимое.
struct StubState {
    tag: &'static str,
    /// Методы, которые стенд обслужил. По ним видно, что запрос ушёл именно
    /// на этот портал, а не на соседний.
    calls: Mutex<Vec<String>>,
}

/// Поднятый стенд портала.
struct PortalStub {
    base_url: String,
    state: Arc<StubState>,
}

impl PortalStub {
    fn calls(&self) -> Vec<String> {
        self.calls_guard().clone()
    }

    fn calls_guard(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        match self.state.calls.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }
}

/// Конверт успешного ответа портала.
fn ok(result: Value) -> Json<Value> {
    Json(json!({
        "result": result,
        "time": {
            "start": 0.0, "finish": 0.0, "duration": 0.0,
            "processing": 0.0, "date_start": "", "date_finish": "",
            "operating": 0.0, "operating_reset_at": 0
        }
    }))
}

/// Строка списка диалогов, помеченная порталом.
fn recent_row(tag: &str) -> Value {
    json!([{
        "id": DIALOG,
        "chat_id": 7,
        "type": "chat",
        "title": format!("Чат портала {tag}"),
        "message": {
            "id": MESSAGE_ID,
            "text": format!("сообщение портала {tag}"),
            "author_id": 15,
            "date": "2026-08-05T20:20:36+03:00",
            "file": false
        },
        "date_update": "2026-08-05T20:20:36+03:00",
        "counter": 0,
        "chat": {"id": 7, "name": format!("Чат портала {tag}"), "type": "chat",
                 "entity_type": "", "text_field_enabled": true},
        "user": {"id": 0}
    }])
}

/// Один метод стенда. Всё, чего нет в списке, — безобидное `true`: этот
/// стенд проверяет адресацию портала, а не поведение платформы (для этого
/// есть `tests/b24_stub.rs`).
async fn handle(
    State(state): State<Arc<StubState>>,
    Path(method_json): Path<String>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    // Клиент зовёт `<method>.json` — так же, как боевой портал (см.
    // `tests/b24_stub.rs`). Без снятия суффикса ВСЕ методы уехали бы в
    // фолбэк-ветку `true`, и тест «изоляции» проходил бы на пустых ответах.
    let method = method_json
        .strip_suffix(".json")
        .unwrap_or(&method_json)
        .to_string();
    let params = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let tag = state.tag;
    match state.calls.lock() {
        Ok(mut g) => g.push(method.clone()),
        Err(p) => p.into_inner().push(method.clone()),
    }

    match method.as_str() {
        // Холодный старт списка едет одним пакетом (Р3).
        "batch" => {
            let mut results = serde_json::Map::new();
            if let Some(cmd) = params["cmd"].as_object() {
                for (key, command) in cmd {
                    let command = command.as_str().unwrap_or("");
                    let method = command.split_once('?').map_or(command, |(m, _)| m);
                    let value = match method {
                        "im.recent.get" => recent_row(tag),
                        "im.counters.get" => json!({
                            "TYPE": {"ALL": 0, "NOTIFY": 0, "CHAT": 0, "DIALOG": 0,
                                     "MESSENGER": 0, "TASKS_TASK": 0, "LINES": 0,
                                     "COPILOT": 0, "COLLAB": 0},
                            "DIALOG": {}, "CHAT": [], "CHAT_UNREAD": []
                        }),
                        _ => json!(true),
                    };
                    results.insert(key.clone(), value);
                }
            }
            ok(json!({
                "result": Value::Object(results),
                "result_error": [], "result_total": [],
                "result_next": [], "result_time": []
            }))
        }
        "im.dialog.messages.get" => ok(json!({
            "chat_id": 7,
            "messages": [{
                "id": MESSAGE_ID,
                "chat_id": 7,
                "author_id": 15,
                "date": "2026-08-05T20:20:36+03:00",
                "text": format!("сообщение портала {tag}"),
                "params": [],
            }],
            "users": [], "files": []
        })),
        "tasks.task.list" => ok(json!({
            "tasks": [{
                "id": TASK_ID.to_string(),
                "title": format!("Задача портала {tag}"),
                "status": "2",
                "priority": "1",
                "createdBy": "1",
                "responsibleId": "1",
                "createdDate": "2026-06-29T14:40:42+03:00",
                "activityDate": "2026-08-05T19:51:04+03:00",
                "groupId": "0",
                "group": [], "auditors": [], "accomplices": [],
                "newCommentsCount": 0,
                "notViewed": "N",
                "action": {"start": true, "pause": false, "complete": true,
                           "defer": true, "renew": false}
            }]
        })),
        "tasks.task.get" => ok(json!({
            "task": {
                "id": TASK_ID.to_string(),
                "title": format!("Задача портала {tag}"),
                "status": "2",
                "priority": "1",
                "createdBy": "1",
                "responsibleId": "1",
                "createdDate": "2026-06-29T14:40:42+03:00",
                "activityDate": "2026-08-05T19:51:04+03:00",
                "groupId": "0",
                "group": [], "auditors": [], "accomplices": [],
                "newCommentsCount": 0,
                "notViewed": "N",
                "action": {"start": true, "pause": false, "complete": true,
                           "defer": true, "renew": false}
            }
        })),
        _ => ok(json!(true)),
    }
}

/// Поднимает стенд портала и возвращает его базовый URL.
async fn start_stub(tag: &'static str) -> PortalStub {
    let state = Arc::new(StubState {
        tag,
        calls: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route(&format!("/rest/1/{CODE}/:method"), post(handle).get(handle))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    PortalStub {
        base_url: format!("http://{addr}/rest/1/{CODE}"),
        state,
    }
}

// ===========================================================================
// Реестр из двух порталов.
// ===========================================================================

/// Плагин с ДВУМЯ подключёнными порталами и роутером поверх реестра.
struct Harness {
    _dir: tempfile::TempDir,
    alpha: PortalStub,
    beta: PortalStub,
    app: Router,
    /// Пути заметок анализа — по одному на портал.
    alpha_state: Arc<B24State>,
    beta_state: Arc<B24State>,
}

/// Готовое к работе состояние портала: свой стенд, свои пути, авторизация
/// пройдена, связь считается живой.
async fn portal_state(dir: &std::path::Path, id: &str, stub: &PortalStub) -> Arc<B24State> {
    let cfg = BitrixConfig {
        // Пути РАЗНЫЕ у разных порталов: до Ф6 их разводит конфигурация, а не
        // код, но проверяемое здесь свойство — маршрутизация запроса в СВОЁ
        // состояние — от этого не зависит.
        cache_dir: dir.join(id).join("cache"),
        prompts_path: dir.join(id).join("prompts.json"),
        claude_cli_path: dir.join("no-such-claude"),
        config_path: dir.join("bitrix.toml"),
        base_url_override: stub.base_url.clone(),
        webhook: WebhookUrl::new(format!("https://{id}.bitrix24.ru/rest/1/{CODE}/")),
        portal_id: id.to_string(),
        connect_timeout_secs: 2,
        request_timeout_secs: 2,
        rate_limit_rps: 50,
        burst: 100,
        ..BitrixConfig::default()
    };
    let state = B24State::new(cfg);
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: format!("{id}.bitrix24.ru"),
        })
        .await;
    state.note_ok(chrono::Utc::now().timestamp()).await;
    state
}

async fn two_portals() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let alpha = start_stub("alpha").await;
    let beta = start_stub("beta").await;

    let alpha_state = portal_state(dir.path(), "alpha", &alpha).await;
    let beta_state = portal_state(dir.path(), "beta", &beta).await;

    let registry = B24Registry::from_states(vec![alpha_state.clone(), beta_state.clone()]);
    let app = forge_bitrix::routes::build_router(registry);

    Harness {
        _dir: dir,
        alpha,
        beta,
        app,
        alpha_state,
        beta_state,
    }
}

// ===========================================================================
// Помощники запроса.
// ===========================================================================

/// Запрос ЛЮБЫМ методом, с необязательным заголовком портала и телом.
async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    portal: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(p) = portal {
        b = b.header("X-Forge-Portal", p);
    }
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let req = b
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(app: &Router, uri: &str, portal: Option<&str>) -> (StatusCode, Value) {
    request(app, "GET", uri, portal, None).await
}

/// Полный текст ответа — по нему проверяется, что чужой метки НЕТ НИГДЕ, а не
/// только в тех полях, которые тест догадался перечислить.
fn text(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// СПИСОК ДИАЛОГОВ: у обоих порталов один `dialog_id`, но своё содержимое.
#[tokio::test]
async fn the_dialog_list_of_one_portal_never_shows_the_other() {
    let h = two_portals().await;

    let (status, a) = get(&h.app, "/api/bitrix/dialogs?limit=50", Some("alpha")).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let (status, b) = get(&h.app, "/api/bitrix/dialogs?limit=50", Some("beta")).await;
    assert_eq!(status, StatusCode::OK, "{b}");

    // Идентификатор диалога СОВПАДАЕТ — различается только содержимое.
    assert_eq!(a["dialogs"][0]["id"], DIALOG);
    assert_eq!(b["dialogs"][0]["id"], DIALOG);

    assert!(text(&a).contains("alpha"), "{a}");
    assert!(!text(&a).contains("beta"), "данные beta в ответе alpha: {a}");
    assert!(text(&b).contains("beta"), "{b}");
    assert!(!text(&b).contains("alpha"), "данные alpha в ответе beta: {b}");

    // И запрос физически ушёл на СВОЙ стенд: каждый обслужил свой холодный
    // старт, и ровно столько же, сколько сосед. (Кроме `batch` холодного
    // старта сюда попадает доборный пакет имён — их число зависит от кэша
    // собеседников, поэтому сравниваются КОЛИЧЕСТВА, а не списки.)
    assert!(!h.alpha.calls().is_empty(), "alpha не получил запроса");
    assert_eq!(
        h.alpha.calls().len(),
        h.beta.calls().len(),
        "порталы обслужили разное число запросов: alpha={:?}, beta={:?}",
        h.alpha.calls(),
        h.beta.calls()
    );
}

/// ЛЕНТА СООБЩЕНИЙ: одинаковые `dialog_id` И `message_id` у обоих порталов.
#[tokio::test]
async fn the_message_feed_of_one_portal_never_shows_the_other() {
    let h = two_portals().await;
    let uri = format!("/api/bitrix/dialogs/{DIALOG}/messages");

    let (status, a) = get(&h.app, &uri, Some("alpha")).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let (status, b) = get(&h.app, &uri, Some("beta")).await;
    assert_eq!(status, StatusCode::OK, "{b}");

    assert_eq!(a["messages"][0]["id"], MESSAGE_ID);
    assert_eq!(b["messages"][0]["id"], MESSAGE_ID);
    assert_eq!(a["messages"][0]["text"], "сообщение портала alpha");
    assert_eq!(b["messages"][0]["text"], "сообщение портала beta");
    assert!(!text(&a).contains("beta"), "{a}");
    assert!(!text(&b).contains("alpha"), "{b}");
}

/// ЗАДАЧИ: список и карточка одной и той же задачи `42` у обоих порталов.
#[tokio::test]
async fn the_task_list_and_card_of_one_portal_never_show_the_other() {
    let h = two_portals().await;

    for (portal, mine, theirs) in [("alpha", "alpha", "beta"), ("beta", "beta", "alpha")] {
        let (status, list) = get(&h.app, "/api/bitrix/tasks", Some(portal)).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(list["tasks"][0]["id"], TASK_ID.to_string());
        assert!(text(&list).contains(mine), "{list}");
        assert!(!text(&list).contains(theirs), "чужая задача в списке: {list}");

        let (status, card) = get(&h.app, &format!("/api/bitrix/tasks/{TASK_ID}"), Some(portal)).await;
        assert_eq!(status, StatusCode::OK, "{card}");
        assert!(text(&card).contains(mine), "{card}");
        assert!(!text(&card).contains(theirs), "чужая карточка: {card}");
    }
}

/// ЗАМЕТКА АНАЛИЗА: локальный роут, до портала не доходящий вовсе.
///
/// Он ценен именно поэтому: сетевые роуты можно было бы «изолировать»
/// случайно — разными `base_url_override`. Здесь же чтение и запись идут
/// ЧЕРЕЗ СОСТОЯНИЕ портала, и промах экстрактора виден сразу.
#[tokio::test]
async fn the_analysis_note_of_one_portal_never_leaks_into_the_other() {
    let h = two_portals().await;
    let uri = format!("/api/bitrix/dialogs/{DIALOG}/prompt");

    let (status, _) = request(
        &h.app,
        "PUT",
        &uri,
        Some("alpha"),
        Some(json!({"prompt": "заметка портала alpha"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, a) = get(&h.app, &uri, Some("alpha")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["prompt"], "заметка портала alpha");

    // У beta тот же `dialog_id` — и пусто.
    let (status, b) = get(&h.app, &uri, Some("beta")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["prompt"], "", "заметка alpha протекла в beta: {b}");
}

/// ФОЛБЭК ДЕТЕРМИНИРОВАН: запрос без заголовка стабильно попадает в ПЕРВЫЙ
/// слот реестра, а не в случайный и не в «первый подключённый».
#[tokio::test]
async fn a_request_without_a_header_always_lands_on_the_first_slot() {
    let h = two_portals().await;

    for _ in 0..5 {
        let (status, body) = get(&h.app, "/api/bitrix/dialogs?limit=50", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(text(&body).contains("alpha"), "{body}");
        assert!(!text(&body).contains("beta"), "{body}");
    }
    assert!(h.beta.calls().is_empty(), "фолбэк сходил на beta");
}

/// `?portal=` работает там, где заголовка быть не может, — и адресует тот же
/// портал, что и заголовок.
#[tokio::test]
async fn the_query_fallback_addresses_the_same_portal_as_the_header() {
    let h = two_portals().await;

    let (status, q) = get(&h.app, "/api/bitrix/dialogs?limit=50&portal=beta", None).await;
    assert_eq!(status, StatusCode::OK, "{q}");
    let (status, hdr) = get(&h.app, "/api/bitrix/dialogs?limit=50", Some("beta")).await;
    assert_eq!(status, StatusCode::OK, "{hdr}");
    assert_eq!(q["dialogs"], hdr["dialogs"]);
    assert!(h.alpha.calls().is_empty(), "запрос к beta сходил на alpha");
}

/// НЕИЗВЕСТНЫЙ ПОРТАЛ — 404 в общем конверте ошибок, а не молчаливый сполз на
/// первый слот. И до портала запрос не доезжает вовсе.
#[tokio::test]
async fn an_unknown_portal_is_404_and_never_reaches_any_portal() {
    let h = two_portals().await;

    for uri in [
        "/api/bitrix/status",
        "/api/bitrix/dialogs?limit=50",
        &format!("/api/bitrix/dialogs/{DIALOG}/messages"),
        "/api/bitrix/tasks",
        &format!("/api/bitrix/tasks/{TASK_ID}"),
        &format!("/api/bitrix/dialogs/{DIALOG}/prompt"),
    ] {
        let (status, body) = get(&h.app, uri, Some("gamma")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "unknown_portal", "{uri}: {body}");
    }

    assert!(h.alpha.calls().is_empty(), "запрос к несуществующему порталу дошёл до alpha");
    assert!(h.beta.calls().is_empty(), "запрос к несуществующему порталу дошёл до beta");
}

/// `/api/bitrix/status` — один роут, два разных ответа: экран подключения
/// каждой вкладки обязан описывать СВОЙ портал.
#[tokio::test]
async fn status_reports_the_state_of_the_addressed_portal() {
    let h = two_portals().await;

    // Разводим состояния автомата: alpha подключён, beta — оборвался.
    h.beta_state
        .note_down(chrono::Utc::now().timestamp(), "connect timeout")
        .await;

    let (status, a) = get(&h.app, "/api/bitrix/status", Some("alpha")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["connected"], true, "{a}");

    let (status, b) = get(&h.app, "/api/bitrix/status", Some("beta")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["connected"], false, "{b}");

    // И ни в одном ответе нет ни хоста портала, ни кода вебхука.
    for v in [&a, &b] {
        let s = text(v);
        assert!(!s.contains(CODE), "код вебхука в /status: {s}");
        // Хост ИМЕННО ЭТИХ порталов; строка `bitrix24.ru` сама по себе в
        // ответе законна — она есть в подсказке экрана подключения.
        assert!(!s.contains("alpha.bitrix24.ru"), "хост портала в /status: {s}");
        assert!(!s.contains("beta.bitrix24.ru"), "хост портала в /status: {s}");
    }
    assert!(h.alpha_state.is_authorized().await);
}

/// ЛЕСТНИЦА НЕ СДВИНУЛАСЬ: резолв портала встал ПЕРЕД ней, но её собственный
/// порядок `409 → 503 → 404 → 400` остался прежним для каждого портала
/// отдельно.
///
/// Проверяется на портале, который НЕ подключён: мусорный `:id` и мусорный
/// `?limit` обязаны получить 409, а не 404 и не 400.
#[tokio::test]
async fn the_check_ladder_still_starts_with_409_per_portal() {
    let h = two_portals().await;
    // beta «разлогинивается» — alpha при этом обязан остаться рабочим.
    h.beta_state.set_login(LoginFlow::NotConfigured).await;

    for uri in [
        "/api/bitrix/dialogs?limit=%D0%BC%D1%83%D1%81%D0%BE%D1%80",
        "/api/bitrix/dialogs/..%2F..%2Fetc%2Fpasswd/messages",
        "/api/bitrix/tasks/%00",
    ] {
        let (status, body) = get(&h.app, uri, Some("beta")).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{uri} обязан отвечать 409 раньше разбора: {body}"
        );
        assert_eq!(body["error"]["code"], "not_authorized", "{uri}: {body}");
    }

    // Соседний портал не пострадал.
    let (status, a) = get(&h.app, "/api/bitrix/dialogs?limit=50", Some("alpha")).await;
    assert_eq!(status, StatusCode::OK, "{a}");
}
