//! Интеграционные тесты задач (Ф10): связь `task_id ↔ chat_id`, матрица
//! действий, пагинация, счётчики, ветка «задача без чата» (Р20).
//!
//! # Свой стенд, а не `b24_stub.rs`
//!
//! Стенд транспорта проверяет ТРАНСПОРТ: лимиты, ретраи, нарезку `batch`,
//! очистку секрета. Здесь нужен стенд, который ведёт себя как ПОРТАЛ С
//! ЗАДАЧАМИ: помнит их статусы, переворачивает матрицу `action` после
//! `tasks.task.start`, пишет в чат задачи системное сообщение и — главное —
//! **точно воспроизводит три ловушки платформы**:
//!
//! | ловушка | как её изображает стенд |
//! |---|---|
//! | `ENTITY_TYPE = "TASKS"` | отвечает `{"result": null}` при HTTP 200 |
//! | camelCase в `select` | поле молча приезжает `null` |
//! | `{"STATUS": …}` в фильтре | возвращает НОЛЬ записей при подходящих задачах |
//!
//! Без такого стенда тесты доказывали бы только то, что код не падает.
//!
//! Порт всегда `127.0.0.1:0` — операционная система выдаёт свободный. Жёсткий
//! порт однажды упёрся бы в живой devforge на 7331.
//!
//! # Что здесь проверяется
//!
//! | тест | инвариант фазы |
//! |---|---|
//! | `the_task_card_carries_its_chat_and_its_action_matrix` | связь `task_id → chat_id` через `chatId` |
//! | `the_reverse_lookup_goes_through_the_entity_type` | обратный поиск через `im.chat.get` |
//! | `the_card_is_fetched_without_a_select` | ловушка 2 на роуте карточки |
//! | `the_page_is_fifty_rows_addressed_by_start` | пагинация `start = (N-1)*50` |
//! | `a_forbidden_action_is_a_409_without_touching_the_portal` | матрица: `false` → 409 и НОЛЬ вызовов |
//! | `an_unsupported_action_is_a_501_not_a_409` | «не умеем» ≠ «сейчас нельзя» |
//! | `a_successful_action_flips_the_matrix_and_names_the_dialog_to_reload` | карточка и лента перезапрашиваются |
//! | `the_system_message_lands_in_the_task_chat` | смена статуса видна в ленте |
//! | `all_six_statuses_and_three_priorities_project_into_the_dto` | таблицы |
//! | `a_task_without_a_chat_never_asks_the_portal_for_one` | Р20 |
//! | `a_chat_that_appears_later_is_picked_up_after_an_action` | Р20, п. 4 |
//! | `the_two_counter_families_never_mix` | `tasks.task.counters.get` ≠ `im.counters.get` |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post as route_post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow};

/// Код вебхука, который стенд обязан увидеть в пути REST-вызова.
const CODE: &str = "wqdzbfhmn83d23j4";

/// Пользователь вебхука.
const UID: &str = "1";

// ===========================================================================
// Стенд портала с задачами.
// ===========================================================================

/// Состояние стенда.
#[derive(Default)]
struct StubState {
    /// Журнал вызовов: имя метода и тело запроса.
    ///
    /// Ровно им доказываются все три ловушки: «с какими параметрами мы на
    /// самом деле сходили» нельзя вывести ни из ответа, ни из кода.
    calls: Mutex<Vec<(String, Value)>>,
    /// `task_id` → карточка (стенд её МЕНЯЕТ при действиях).
    tasks: Mutex<HashMap<i64, Value>>,
    /// `chat_id` → сообщения ленты (системные пишет сам стенд).
    chats: Mutex<HashMap<i64, Vec<Value>>>,
    /// Ответ `tasks.task.counters.get`.
    task_counters: Mutex<Value>,
    /// Ответ `im.counters.get`.
    im_counters: Mutex<Value>,
}

impl StubState {
    fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn count_of(&self, method: &str) -> usize {
        self.calls_of(method).len()
    }

    fn total_calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn task(&self, id: i64) -> Value {
        self.tasks.lock().unwrap().get(&id).cloned().unwrap_or(Value::Null)
    }

    fn set_task(&self, id: i64, task: Value) {
        self.tasks.lock().unwrap().insert(id, task);
    }

    /// Меняет статус задачи и ПЕРЕВОРАЧИВАЕТ матрицу — ровно как портал.
    fn apply_action(&self, id: i64, action: &str) -> bool {
        let mut guard = self.tasks.lock().unwrap();
        let Some(task) = guard.get_mut(&id) else {
            return false;
        };
        let (status, matrix) = match action {
            "start" => (3, json!({"start": false, "pause": true, "complete": true, "defer": false, "renew": false})),
            "pause" => (2, json!({"start": true, "pause": false, "complete": true, "defer": true, "renew": false})),
            "complete" => (5, json!({"start": false, "pause": false, "complete": false, "defer": false, "renew": true})),
            "defer" => (6, json!({"start": true, "pause": false, "complete": true, "defer": false, "renew": true})),
            "renew" => (2, json!({"start": true, "pause": false, "complete": true, "defer": true, "renew": false})),
            _ => return false,
        };
        task["status"] = json!(status.to_string());
        task["action"] = matrix;
        let chat = task["chatId"].as_i64().or_else(|| {
            task["chatId"].as_str().and_then(|s| s.parse::<i64>().ok())
        });
        drop(guard);
        // Портал пишет в чат задачи системное сообщение о смене статуса
        // (`author_id: 0`). Именно оно и делает ленту задачи самодостаточной.
        if let Some(chat) = chat.filter(|v| *v > 0) {
            let mut chats = self.chats.lock().unwrap();
            let list = chats.entry(chat).or_default();
            let next = list.iter().filter_map(|m| m["id"].as_i64()).max().unwrap_or(5) + 2;
            list.push(json!({
                "id": next,
                "chat_id": chat,
                "author_id": 0,
                "date": "2026-08-06T09:00:00+03:00",
                "text": format!("[USER=1]Игорь Герасимов[/USER] — действие «{action}» по задаче"),
                "params": []
            }));
        }
        true
    }
}

/// Живая карточка тестового портала (задача 1, чат 7).
fn live_task(id: i64, chat_id: Value) -> Value {
    json!({
        "id": id.to_string(),
        "title": format!("Задача {id}"),
        "description": "Из встречи [B]«Совещание по запуску продукта»[/B]",
        "descriptionInBbcode": "Y",
        "priority": "1",
        "status": "2",
        "subStatus": "-1",
        "createdBy": "1",
        "responsibleId": "1",
        "createdDate": "2026-06-29T14:40:42+03:00",
        "deadline": "2026-07-04T19:00:00+03:00",
        "activityDate": "2026-08-05T19:51:04+03:00",
        "groupId": "0",
        "group": [],
        "auditors": [],
        "accomplices": [],
        "chatId": chat_id,
        "newCommentsCount": 0,
        "notViewed": "N",
        "creator": {"id": "1", "name": "Игорь Герасимов", "link": "/company/personal/user/1/"},
        "responsible": {"id": "1", "name": "Игорь Герасимов", "link": "/company/personal/user/1/"},
        "action": {
            "complete": true, "start": true, "pause": false, "delegate": true,
            "defer": true, "renew": false, "edit": true, "remove": true,
            "changeDeadline": true, "rate": true, "take": false, "approve": false,
            "accept": false, "decline": false, "checklist.add": true
        }
    })
}

/// Поднятый стенд.
struct Stub {
    base_url: String,
    state: Arc<StubState>,
}

async fn start_stub() -> Stub {
    let state = Arc::new(StubState {
        task_counters: Mutex::new(json!({
            "expired": {"counter": 2, "code": 6291456},
            "new_comments": {"counter": 3, "code": 12582912},
            "mentioned": {"counter": 1, "code": 15728640}
        })),
        im_counters: Mutex::new(json!({
            "TYPE": {"ALL": 9, "NOTIFY": 5, "CHAT": 0, "DIALOG": 4,
                     "MESSENGER": 4, "TASKS_TASK": 7},
            "DIALOG": {"19": 1},
            "CHAT": [], "CHAT_UNREAD": []
        })),
        ..StubState::default()
    });
    let app = Router::new()
        .route("/rest/:uid/:code/:method", route_post(handle))
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

/// Успешный конверт портала.
fn ok(result: Value) -> Response {
    Json(json!({"result": result, "time": {"operating": 0.0}})).into_response()
}

/// Отказ портала: HTTP 200 с телом ошибки — самая частая форма.
fn err(code: &str) -> Response {
    Json(json!({"error": code, "error_description": format!("stub: {code}")})).into_response()
}

async fn handle(
    State(state): State<Arc<StubState>>,
    Path((_uid, code, method_json)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Response {
    assert_eq!(code, CODE, "клиент собрал URL с чужим кодом");
    let method = method_json.trim_end_matches(".json").to_string();
    let params: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state
        .calls
        .lock()
        .unwrap()
        .push((method.clone(), params.clone()));

    match method.as_str() {
        "tasks.task.list" => task_list(&state, &params),
        "tasks.task.get" => {
            let id = params["taskId"].as_i64().unwrap_or_default();
            let task = state.task(id);
            if task.is_null() {
                // ЖИВОЕ ПОВЕДЕНИЕ ПОРТАЛА: несуществующая задача — это не
                // отказ, а УСПЕХ с пустым PHP-массивом (проверено на
                // `taskId: 999999`). Ответить здесь ошибкой значило бы
                // проверять выдуманное поведение.
                return ok(json!([]));
            }
            ok(json!({"task": select_fields(&task, &params["select"])}))
        }
        "tasks.task.counters.get" => ok(state.task_counters.lock().unwrap().clone()),
        "im.counters.get" => ok(state.im_counters.lock().unwrap().clone()),
        "im.chat.get" => chat_get(&state, &params),
        "im.dialog.messages.get" => {
            let dialog = params["DIALOG_ID"].as_str().unwrap_or("");
            let chat = dialog.strip_prefix("chat").and_then(|s| s.parse::<i64>().ok());
            let msgs = chat
                .and_then(|c| state.chats.lock().unwrap().get(&c).cloned())
                .unwrap_or_default();
            ok(json!({"messages": msgs, "users": [], "files": []}))
        }
        "batch" => batch(&state, &params),
        "im.recent.get" => ok(json!([])),
        m if m.starts_with("tasks.task.") => {
            let action = m.trim_start_matches("tasks.task.");
            let id = params["taskId"].as_i64().unwrap_or_default();
            if state.apply_action(id, action) {
                ok(json!(true))
            } else {
                err("ACCESS_DENIED")
            }
        }
        _ => ok(json!(true)),
    }
}

/// `batch` — демультиплексор пакета: команды выполняются тем же обработчиком,
/// а ответы раскладываются по ИСХОДНЫМ ключам.
///
/// Нужен фолбэк-воркеру: счётчики задач едут ТРЕТЬЕЙ командой того же пакета,
/// и без разбора `batch` проверить это было бы нечем.
fn batch(state: &Arc<StubState>, params: &Value) -> Response {
    let mut results = serde_json::Map::new();
    if let Some(cmd) = params["cmd"].as_object() {
        for (key, command) in cmd {
            let command = command.as_str().unwrap_or("");
            let (method, query) = command.split_once('?').unwrap_or((command, ""));
            // Вложенные команды приезжают urlencode'нутыми — нам хватит
            // `userId`, остальное воркеру не важно.
            let mut nested = serde_json::Map::new();
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                if let Some((k, v)) = pair.split_once('=') {
                    if let Ok(n) = v.parse::<i64>() {
                        nested.insert(k.to_string(), json!(n));
                    } else {
                        nested.insert(k.to_string(), json!(v));
                    }
                }
            }
            let nested = Value::Object(nested);
            state
                .calls
                .lock()
                .unwrap()
                .push((method.to_string(), nested.clone()));
            let value = match method {
                "im.recent.get" => json!([]),
                "im.counters.get" => state.im_counters.lock().unwrap().clone(),
                "tasks.task.counters.get" => state.task_counters.lock().unwrap().clone(),
                _ => json!(true),
            };
            results.insert(key.clone(), value);
        }
    }
    ok(json!({
        "result": Value::Object(results),
        "result_error": [],
        "result_total": [],
        "result_next": [],
        "result_time": []
    }))
}

/// `tasks.task.list` со всеми тремя ловушками платформы.
fn task_list(state: &Arc<StubState>, params: &Value) -> Response {
    let filter = &params["filter"];

    // ЛОВУШКА 3: `STATUS` — псевдополе. Портал не ругается, он просто
    // возвращает ноль записей.
    if filter.get("STATUS").is_some() {
        return ok(json!({"tasks": []}));
    }

    let mut all: Vec<Value> = state.tasks.lock().unwrap().values().cloned().collect();
    all.sort_by_key(|t| std::cmp::Reverse(t["id"].as_str().unwrap_or("0").parse::<i64>().unwrap_or(0)));

    // Фильтр по РЕАЛЬНОМУ статусу — тот, что работает.
    let keep = |t: &Value| -> bool {
        let status = t["status"].as_str().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let mut ok = true;
        if let Some(v) = filter.get("REAL_STATUS").and_then(Value::as_i64) {
            ok &= status == v;
        }
        if let Some(v) = filter.get("<REAL_STATUS").and_then(Value::as_i64) {
            ok &= status < v;
        }
        ok
    };
    let filtered: Vec<Value> = all.into_iter().filter(keep).collect();
    let total = filtered.len() as i64;

    let start = params["start"].as_i64().unwrap_or(0).max(0) as usize;
    let page: Vec<Value> = filtered
        .into_iter()
        .skip(start)
        // Страница у портала ФИКСИРОВАНА пятьюдесятью записями.
        .take(50)
        .map(|t| select_fields(&t, &params["select"]))
        // В списке `action` не приходит вовсе — только в карточке.
        .map(|mut t| {
            if let Some(o) = t.as_object_mut() {
                o.remove("action");
            }
            t
        })
        .collect();

    Json(json!({"result": {"tasks": page}, "total": total, "time": {"operating": 0.0}}))
        .into_response()
}

/// `im.chat.get` — ГЛАВНАЯ ЛОВУШКА ЗАДАЧ вживую.
///
/// `ENTITY_TYPE = "TASKS"` даёт `{"result": null}` при HTTP 200: не ошибку, не
/// пустой объект, а именно `null`. Стенд обязан воспроизводить это дословно —
/// иначе тест-страж проверял бы выдуманное поведение.
fn chat_get(state: &Arc<StubState>, params: &Value) -> Response {
    let entity_type = params["ENTITY_TYPE"].as_str().unwrap_or("");
    if entity_type != "TASKS_TASK" {
        return ok(Value::Null);
    }
    let id = params["ENTITY_ID"]
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| params["ENTITY_ID"].as_i64())
        .unwrap_or_default();
    let task = state.task(id);
    let chat = task["chatId"]
        .as_i64()
        .or_else(|| task["chatId"].as_str().and_then(|s| s.parse::<i64>().ok()))
        .filter(|v| *v > 0);
    match chat {
        Some(c) => ok(json!({"ID": c})),
        None => ok(Value::Null),
    }
}

/// ЛОВУШКА 2: `select` фильтрует поля, и camelCase-имя портал МОЛЧА
/// игнорирует.
///
/// Ответ при этом остаётся успешным, а поле просто отсутствует — то есть
/// приезжает `null` и неотличимо от настоящего `null`. Стенд воспроизводит
/// ровно это: имя из `select` переводится из `UPPER_SNAKE` в `camelCase`, и
/// если переданное имя уже camelCase — совпадения не будет, поле выпадет.
fn select_fields(task: &Value, select: &Value) -> Value {
    let Some(names) = select.as_array() else {
        // Без `select` портал отдаёт ВСЁ — ровно поэтому карточка зовётся без
        // него.
        return task.clone();
    };
    let mut out = serde_json::Map::new();
    // `id` портал отдаёт всегда.
    out.insert("id".into(), task["id"].clone());
    for name in names {
        let Some(name) = name.as_str() else { continue };
        // Верное имя — UPPERCASE; camelCase просто не находит поля.
        if name.chars().any(|c| c.is_ascii_lowercase()) {
            continue;
        }
        let camel = upper_snake_to_camel(name);
        if let Some(v) = task.get(&camel) {
            out.insert(camel, v.clone());
        }
    }
    Value::Object(out)
}

/// `CHAT_ID` → `chatId`.
fn upper_snake_to_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for (i, ch) in name.chars().enumerate() {
        if ch == '_' {
            upper_next = true;
            continue;
        }
        if i == 0 || !upper_next {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch.to_ascii_uppercase());
            upper_next = false;
        }
    }
    out
}

// ===========================================================================
// Обвязка.
// ===========================================================================

/// Стенд + состояние + роутер плагина, подключённый к стенду.
async fn harness() -> (tempfile::TempDir, Arc<StubState>, Arc<B24State>, Router) {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        webhook: WebhookUrl::new(format!("https://test-work-2.bitrix24.ru/rest/{UID}/{CODE}/")),
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
            portal: "test-work-2.bitrix24.ru".into(),
        })
        .await;
    let router = forge_bitrix::routes::build_router(state.clone());
    (dir, stub.state, state, router)
}

async fn http_get(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, "GET", uri).await
}

async fn http_post(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, "POST", uri).await
}

async fn send(app: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// СВЯЗЬ `task_id → chat_id`: она приезжает полем `chatId` карточки, и из неё
/// собирается `DIALOG_ID` ленты.
#[tokio::test]
async fn the_task_card_carries_its_chat_and_its_action_matrix() {
    let (_d, stub, state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));

    let (status, v) = http_get(&app, "/api/bitrix/tasks/1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["id"], "1");
    assert_eq!(v["chat_id"], 7);
    assert_eq!(v["dialog_id"], "chat7", "DIALOG_ID = \"chat\" + chatId");
    assert_eq!(v["status"], 2);
    assert_eq!(v["status_label"], "Ждёт выполнения");
    // Кнопки — СТРОГО по флагам `true` и только выполнимые панелью.
    assert_eq!(v["actions"], json!(["complete", "defer", "start"]));
    // Ссылка «открыть в Битриксе» — абсолютная и без кода вебхука.
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("https://test-work-2.bitrix24.ru/"), "{url}");
    assert!(!url.contains(CODE), "код вебхука в ссылке: {url}");

    // Индекс запомнил связь в обе стороны.
    assert_eq!(state.task_of_chat(7), Some(1));
    assert_eq!(state.task_entry(1).unwrap().chat_id, Some(7));
}

/// ОБРАТНЫЙ ПОИСК: `im.chat.get {ENTITY_TYPE:"TASKS_TASK", ENTITY_ID}`.
///
/// Значение сущности передать неправильно невозможно — обёртка не принимает
/// его параметром вовсе. Здесь проверяется, что журнал стенда видит именно
/// `TASKS_TASK` и что ответ разбирается в идентификатор чата.
#[tokio::test]
async fn the_reverse_lookup_goes_through_the_entity_type() {
    let (_d, stub, state, _app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));

    let client = state.api_client().await.unwrap();
    assert_eq!(client.task_chat_id(1).await.unwrap(), Some(7));

    let calls = stub.calls_of("im.chat.get");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["ENTITY_TYPE"], "TASKS_TASK");
    assert_eq!(calls[0]["ENTITY_ID"], "1");
}

/// ЛОВУШКА 2 НА РОУТЕ КАРТОЧКИ: `tasks.task.get` зовётся БЕЗ `select`.
///
/// Со `select` пришлось бы перечислять полсотни имён в правильном регистре, а
/// одна опечатка дала бы не ошибку, а `null` в поле — то есть карточку без
/// чата и без матрицы, неотличимую от честной.
#[tokio::test]
async fn the_card_is_fetched_without_a_select() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));

    let (status, v) = http_get(&app, "/api/bitrix/tasks/1").await;
    assert_eq!(status, StatusCode::OK);
    // Карточка полна: и чат, и матрица на месте.
    assert_eq!(v["chat_id"], 7);
    assert!(!v["actions"].as_array().unwrap().is_empty());

    let calls = stub.calls_of("tasks.task.get");
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].get("select").is_none(),
        "карточка зовётся БЕЗ select: {}",
        calls[0]
    );
}

/// ПАГИНАЦИЯ: страница пятьдесят, смещение `start = (N-1)*50`, `total` рядом с
/// `result`.
#[tokio::test]
async fn the_page_is_fifty_rows_addressed_by_start() {
    let (_d, stub, _state, app) = harness().await;
    for id in 1..=120 {
        stub.set_task(id, live_task(id, json!(1000 + id)));
    }

    let (status, v) = http_get(&app, "/api/bitrix/tasks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["tasks"].as_array().unwrap().len(), 50);
    assert_eq!(v["total"], 120, "total лежит рядом с result, а не внутри");
    assert_eq!(v["start"], 0);
    assert_eq!(v["has_more"], true);

    let (_, v) = http_get(&app, "/api/bitrix/tasks?start=100").await;
    assert_eq!(v["start"], 100);
    assert_eq!(v["tasks"].as_array().unwrap().len(), 20);
    assert_eq!(v["has_more"], false);

    // Смещение «полтора экрана» защёлкивается на границу страницы: у метода
    // нет `LIMIT`, и произвольный `start` дал бы строки, уже показанные выше.
    let (_, v) = http_get(&app, "/api/bitrix/tasks?start=137").await;
    assert_eq!(v["start"], 100);
    let sent = stub.calls_of("tasks.task.list");
    assert_eq!(sent.last().unwrap()["start"], 100);
}

/// Страница списка приезжает с `select` из UPPERCASE-имён — и `chatId` в ней
/// есть. Это и есть проверка того, что ловушка 2 обойдена: с camelCase-именем
/// поле пришло бы `null`, и лента задачи не открылась бы из списка.
#[tokio::test]
async fn the_list_select_actually_brings_the_chat_id() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(3, live_task(3, json!("9")));

    let (status, v) = http_get(&app, "/api/bitrix/tasks").await;
    assert_eq!(status, StatusCode::OK);
    let row = &v["tasks"][0];
    assert_eq!(row["id"], "3");
    assert_eq!(row["chat_id"], 9, "chatId выпал бы при camelCase в select");
    assert_eq!(row["dialog_id"], "chat9");
}

/// МАТРИЦА: действие с флагом `false` — это `409 invalid_state` и НОЛЬ вызовов
/// портала.
///
/// Карточка уже прочитана (она на экране), значит матрица известна: спрашивать
/// её второй раз, чтобы ответить «нельзя», означало бы тратить один из двух
/// запросов в секунду впустую.
#[tokio::test]
async fn a_forbidden_action_is_a_409_without_touching_the_portal() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));

    // Карточка прочитана — матрица в индексе.
    assert_eq!(http_get(&app, "/api/bitrix/tasks/1").await.0, StatusCode::OK);
    let before = stub.total_calls();

    // `pause` у ждущей задачи — `false`.
    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/pause").await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "invalid_state");
    assert_eq!(
        stub.total_calls(),
        before,
        "запрещённое действие не имеет права стоить ни одного запроса к порталу"
    );
}

/// «Панель этого не умеет» — это `501`, а не `409`.
///
/// Разница ведёт пользователя в разные места: `409` означает «сделайте
/// сначала что-то другое», `501` — «откройте задачу в Битриксе». `delegate`
/// разрешён порталом (`true`), но требует выбора сотрудника, которого панель
/// сделать не может.
#[tokio::test]
async fn an_unsupported_action_is_a_501_not_a_409() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));
    assert_eq!(http_get(&app, "/api/bitrix/tasks/1").await.0, StatusCode::OK);
    let before = stub.total_calls();

    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/delegate").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{v}");
    assert_eq!(v["error"]["code"], "method_unavailable");
    assert_eq!(stub.total_calls(), before);

    // Мусорное имя действия — это разбор параметра, то есть 400.
    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/..%2F..%2Fetc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "invalid_input");
}

/// ПОСЛЕ ДЕЙСТВИЯ карточка перечитывается, матрица переворачивается, а ответ
/// называет диалог, чью ленту надо перезапросить.
#[tokio::test]
async fn a_successful_action_flips_the_matrix_and_names_the_dialog_to_reload() {
    let (_d, stub, state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));
    assert_eq!(http_get(&app, "/api/bitrix/tasks/1").await.0, StatusCode::OK);

    let mut rx = state.subscribe();

    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/start").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["action"], "start");
    assert_eq!(v["ok"], true);
    // Карточка СВЕЖАЯ: статус уже «выполняется», матрица уже другая.
    assert_eq!(v["task"]["status"], 3);
    assert_eq!(v["task"]["status_label"], "Выполняется");
    assert_eq!(v["task"]["actions"], json!(["complete", "pause"]));
    // Лента: портал написал в этот чат системное сообщение.
    assert_eq!(v["dialog_id"], "chat7");

    // Портал получил именно семантический метод.
    assert_eq!(stub.count_of("tasks.task.start"), 1);
    // И карточка была перечитана ПОСЛЕ действия.
    assert_eq!(stub.count_of("tasks.task.get"), 2);

    // Всем вкладкам уехал кадр `task_update` со свежей карточкой.
    let frame = rx.try_recv().expect("кадр task_update не разослан");
    match frame {
        forge_bitrix::ws::protocol::B24ServerMsg::TaskUpdate { task_id, task } => {
            assert_eq!(task_id, "1");
            assert_eq!(task.status, 3);
            assert_eq!(task.actions, vec!["complete", "pause"]);
        }
        other => panic!("ожидался task_update, приехало {other:?}"),
    }

    // Индекс тоже обновлён: теперь запрещён `start`, а не `pause`.
    let entry = state.task_entry(1).unwrap();
    assert!(!entry.actions.allows("start"));
    assert!(entry.actions.allows("pause"));

    // Обратная сторона: теперь `pause` разрешён и проходит.
    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/pause").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["task"]["status"], 2);
}

/// Смена статуса ВИДНА В ЛЕНТЕ: портал пишет в чат задачи системное сообщение
/// (`author_id: 0`), и лента задачи от этого самодостаточна.
#[tokio::test]
async fn the_system_message_lands_in_the_task_chat() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));
    assert_eq!(http_get(&app, "/api/bitrix/tasks/1").await.0, StatusCode::OK);

    let (_, before) = http_get(&app, "/api/bitrix/dialogs/chat7/messages").await;
    assert!(before["messages"].as_array().unwrap().is_empty());

    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/start").await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let (_, after) = http_get(&app, "/api/bitrix/dialogs/chat7/messages").await;
    let msgs = after["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "системное сообщение о смене статуса");
    assert_eq!(msgs[0]["system"], true, "author_id == 0 ⇒ системное");
    assert!(msgs[0]["text"].as_str().unwrap().contains("start"));
}

/// Все шесть боевых статусов и три приоритета проецируются в DTO.
#[tokio::test]
async fn all_six_statuses_and_three_priorities_project_into_the_dto() {
    let (_d, stub, _state, app) = harness().await;
    let table = [
        (2, "Ждёт выполнения", "pending"),
        (3, "Выполняется", "in_progress"),
        (4, "Ожидает контроля", "review"),
        (5, "Завершена", "completed"),
        (6, "Отложена", "deferred"),
        (7, "Отклонена", "declined"),
    ];
    for (status, label, key) in table {
        let mut task = live_task(status, json!(status + 100));
        task["status"] = json!(status.to_string());
        stub.set_task(status, task);

        let (code, v) = http_get(&app, &format!("/api/bitrix/tasks/{status}")).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["status"], status, "код {status}");
        assert_eq!(v["status_label"], label, "код {status}");
        assert_eq!(v["status_key"], key, "код {status}");
    }

    for (priority, label, key) in [(0, "низкий", "low"), (1, "средний", "normal"), (2, "высокий", "high")] {
        let mut task = live_task(50 + priority, json!(0));
        task["priority"] = json!(priority.to_string());
        stub.set_task(50 + priority, task);

        let (_, v) = http_get(&app, &format!("/api/bitrix/tasks/{}", 50 + priority)).await;
        assert_eq!(v["priority"], priority);
        assert_eq!(v["priority_label"], label);
        assert_eq!(v["priority_key"], key);
    }
}

/// Р20: ЗАДАЧА БЕЗ ЧАТА. Карточка отдаётся целиком, `chat_id` и `dialog_id`
/// пусты, а `im.chat.get` НЕ вызывается вовсе.
///
/// Портал отдаёт отсутствие чата тремя формами — нулём, `null` и строкой
/// `"0"`, — и все три обязаны означать одно.
#[tokio::test]
async fn a_task_without_a_chat_never_asks_the_portal_for_one() {
    for (i, form) in [json!(0), json!(null), json!("0")].into_iter().enumerate() {
        let (_d, stub, state, app) = harness().await;
        let id = 5 + i as i64;
        stub.set_task(id, live_task(id, form.clone()));

        let (status, v) = http_get(&app, &format!("/api/bitrix/tasks/{id}")).await;
        assert_eq!(status, StatusCode::OK, "форма {form}: {v}");
        assert_eq!(v["chat_id"], Value::Null, "форма {form}");
        assert_eq!(v["dialog_id"], Value::Null, "форма {form}");
        // Карточка при этом ПОЛНОЦЕННА: заголовок, статус, кнопки на месте.
        assert_eq!(v["status"], 2);
        assert_eq!(v["actions"], json!(["complete", "defer", "start"]));

        assert_eq!(
            stub.count_of("im.chat.get"),
            0,
            "форма {form}: у задачи без обсуждения чат не спрашивают"
        );
        assert_eq!(state.task_entry(id).unwrap().chat_id, None);
    }
}

/// Р20, п. 4: ЧАТ ПОЯВИЛСЯ ПОЗЖЕ.
///
/// Первое же действие над свежей задачей пишет в её чат системное сообщение,
/// то есть создаёт чат. Если карточка о нём ещё не знает (портал обновляет
/// поле не мгновенно), чат добирается вторым путём — `im.chat.get` с
/// правильной сущностью. Без этого пользователь остался бы без ленты до
/// перезагрузки вкладки.
#[tokio::test]
async fn a_chat_that_appears_later_is_picked_up_after_an_action() {
    let (_d, stub, state, app) = harness().await;
    stub.set_task(5, live_task(5, json!(0)));

    let (status, v) = http_get(&app, "/api/bitrix/tasks/5").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["dialog_id"], Value::Null);
    assert_eq!(stub.count_of("im.chat.get"), 0);

    // Стенд «создаёт» чат в момент действия, но карточку обновляет с
    // опозданием: `chatId` в ней всё ещё ноль.
    let (status, v) = http_post(&app, "/api/bitrix/tasks/5/start").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["task"]["chat_id"], Value::Null, "портал ещё не обновил поле");
    // …а вот тут второй путь и срабатывает: чата нет — спрашиваем сущность.
    assert_eq!(stub.count_of("im.chat.get"), 1);
    assert_eq!(
        stub.calls_of("im.chat.get")[0]["ENTITY_TYPE"],
        "TASKS_TASK",
        "сущность обязана быть TASKS_TASK — на \"TASKS\" портал ответит null"
    );
    // Чата у стенда всё-таки нет — ответ честно пуст, ветка Р20 держится.
    assert_eq!(v["dialog_id"], Value::Null);
    assert_eq!(state.task_entry(5).unwrap().chat_id, None);
}

/// Р20 в списке: задача без чата остаётся В СПИСКЕ и не ломает страницу.
#[tokio::test]
async fn a_chatless_task_still_shows_up_in_the_list() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));
    stub.set_task(5, live_task(5, json!(0)));

    let (status, v) = http_get(&app, "/api/bitrix/tasks").await;
    assert_eq!(status, StatusCode::OK);
    let rows = v["tasks"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let chatless = rows.iter().find(|r| r["id"] == "5").unwrap();
    assert_eq!(chatless["dialog_id"], Value::Null);
    assert_eq!(stub.count_of("im.chat.get"), 0);
}

/// СЧЁТЧИКИ: два независимых семейства, которые нельзя складывать.
///
/// `tasks.task.counters.get` считает СОСТОЯНИЕ задач (просрочена, новые
/// комментарии, упоминания), `im.counters.get` — НЕПРОЧИТАННЫЕ СООБЩЕНИЯ чатов
/// задач (`TYPE.TASKS_TASK`). Смешать их — значит показать бейдж, который
/// нельзя обнулить ни чтением, ни закрытием задачи.
#[tokio::test]
async fn the_two_counter_families_never_mix() {
    let (_d, stub, _state, app) = harness().await;

    let (status, v) = http_get(&app, "/api/bitrix/tasks/counters").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["expired"], 2);
    assert_eq!(v["new_comments"], 3);
    assert_eq!(v["mentioned"], 1);
    // Непрочитанное чатов задач — ДРУГОЕ число, из другого метода.
    assert_eq!(v["unread"], 7);

    // Оба метода действительно вызваны, и `userId` у задач обязателен.
    assert_eq!(stub.count_of("tasks.task.counters.get"), 1);
    assert_eq!(stub.calls_of("tasks.task.counters.get")[0]["userId"], 1);
    assert_eq!(stub.count_of("im.counters.get"), 1);

    // Директория `~/tasks` берёт своё число из `im.counters.get`, а не из
    // счётчиков задач: она показывает непрочитанные СООБЩЕНИЯ.
    let (_, filters) = http_get(&app, "/api/bitrix/filters").await;
    let tasks_dir = filters["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "tasks")
        .unwrap();
    assert_eq!(tasks_dir["unread"], 7);
}

/// Отказ портала на действии доезжает по замороженной таблице ошибок.
#[tokio::test]
async fn a_portal_refusal_maps_through_the_frozen_table() {
    let (_d, stub, _state, app) = harness().await;
    stub.set_task(1, live_task(1, json!(7)));
    assert_eq!(http_get(&app, "/api/bitrix/tasks/1").await.0, StatusCode::OK);
    // Стенд «теряет» задачу — действие получит ACCESS_DENIED.
    stub.tasks.lock().unwrap().remove(&1);

    let (status, v) = http_post(&app, "/api/bitrix/tasks/1/start").await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "send_forbidden");
}

/// Несуществующая задача — `404 unknown_task`, а не пустая карточка.
#[tokio::test]
async fn a_missing_task_is_a_404() {
    let (_d, _stub, _state, app) = harness().await;
    let (status, v) = http_get(&app, "/api/bitrix/tasks/777").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "unknown_task");
}

/// БЕЙДЖИ ЖИВУТ САМИ: фолбэк-воркер тянет ОБА семейства счётчиков ОДНИМ
/// пакетом и рассылает кадр `counters` — без перезагрузки вкладки.
///
/// Третья команда едет в тот же `batch` намеренно: внешний пакет стоит один
/// хит лимита интенсивности независимо от числа команд внутри, поэтому
/// счётчики задач достаются бесплатно. Отдельный запрос стоил бы половины
/// бюджета в 2 rps каждую минуту — ради двух чисел в бейдже.
#[tokio::test]
async fn the_slow_worker_brings_both_counter_families_in_one_batch() {
    let (_d, stub, state, _app) = harness().await;
    let client = state.api_client().await.unwrap();
    let mut rx = state.subscribe();

    let outcome = forge_bitrix::recent_sync::sync_once(&state, &client, None)
        .await
        .expect("проход синхронизации");
    assert!(outcome.counters_changed, "счётчики обязаны приехать");

    // ОДИН запрос к порталу на оба семейства.
    assert_eq!(stub.count_of("batch"), 1);
    assert_eq!(stub.count_of("im.counters.get"), 1, "внутри пакета");
    assert_eq!(stub.count_of("tasks.task.counters.get"), 1, "внутри пакета");
    assert_eq!(
        stub.calls_of("tasks.task.counters.get")[0]["userId"],
        1,
        "у счётчиков задач userId ОБЯЗАТЕЛЕН"
    );

    // Кадр несёт оба семейства и НЕ складывает их.
    let frame = loop {
        match rx.try_recv().expect("кадр counters не разослан") {
            forge_bitrix::ws::protocol::B24ServerMsg::Counters {
                total,
                tasks,
                expired,
                new_comments,
                mentioned,
                ..
            } => break (total, tasks, expired, new_comments, mentioned),
            _ => continue,
        }
    };
    let (total, tasks, expired, new_comments, mentioned) = frame;
    assert_eq!(total, 9, "TYPE.ALL");
    assert_eq!(tasks, 7, "непрочитанные СООБЩЕНИЯ чатов задач");
    assert_eq!(expired, 2, "просроченные ЗАДАЧИ — другое семейство");
    assert_eq!(new_comments, 3);
    assert_eq!(mentioned, 1);
    assert_ne!(tasks, expired, "два семейства не должны совпасть случайно");

    // Снимок остался в состоянии: следующий `GET /tasks/counters` бесплатен.
    let snapshot = state
        .task_counters_fresh(chrono::Utc::now().timestamp(), 60)
        .await
        .expect("снимок счётчиков задач");
    assert_eq!(snapshot.expired, 2);
}

/// Снимок фолбэк-воркера избавляет роут счётчиков от запроса к порталу.
#[tokio::test]
async fn a_fresh_snapshot_costs_the_counters_route_nothing() {
    let (_d, stub, state, app) = harness().await;
    let client = state.api_client().await.unwrap();
    forge_bitrix::recent_sync::sync_once(&state, &client, None)
        .await
        .unwrap();
    let before = stub.total_calls();

    let (status, v) = http_get(&app, "/api/bitrix/tasks/counters").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["expired"], 2);
    assert_eq!(v["unread"], 7);
    assert_eq!(
        stub.total_calls(),
        before,
        "свежий снимок не должен стоить ни одного запроса"
    );
}
