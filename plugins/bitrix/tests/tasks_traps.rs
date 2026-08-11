//! ТРИ ТЕСТА-СТРАЖА ЛОВУШЕК ЗАДАЧ Bitrix24 (Ф10).
//!
//! # Почему эти три ошибки нужны стражи, а другим — нет
//!
//! Обычная ошибка интеграции даёт ОТКАЗ: код, статус, строку в логе. Эти три
//! дают **МОЛЧАЛИВУЮ ПУСТОТУ** — успешный HTTP 200, внутри которого ничего
//! нет. Снаружи это неотличимо от честного «данных нет»:
//!
//! | № | ошибка | что видит пользователь | что видит разработчик |
//! |---|---|---|---|
//! | 1 | `ENTITY_TYPE = "TASKS"` вместо `"TASKS_TASK"` | у задач никогда не открывается лента | `{"result":null}`, HTTP 200 |
//! | 2 | camelCase в `select` (`chatId` вместо `CHAT_ID`) | у задач нет чатов, сроков, статусов | поле `null` — как настоящий `null` |
//! | 3 | ключ `STATUS` в фильтре вместо `REAL_STATUS` | директория задач пуста | `total: 0`, HTTP 200 |
//!
//! Ни одна из трёх не порождает ни исключения, ни кода ошибки, ни записи в
//! логе. Поймать их можно только там, где известно, КАК ИМЕННО мы спросили, —
//! то есть в журнале стенда и в исходниках. Ровно это и делают стражи.
//!
//! # Два вида проверки, и оба нужны
//!
//! 1. **По журналу стенда** — ловит регрессию в РАНТАЙМЕ: код мог собрать
//!    неверный параметр по любой ветке.
//! 2. **По исходникам** (построчный обход `src/`) — ловит регрессию В МОМЕНТ
//!    ПОЯВЛЕНИЯ, даже если ветку ещё никто не вызывает. Приём тот же, что у
//!    стража `no_source_line_logs_the_exposed_url` (Ф2): грубо, зато не
//!    требует запуска.
//!
//! Каждый страж обязан ПАДАТЬ при умышленном возврате к неверному варианту —
//! и в конце файла на это есть отдельные проверки на «отравленных» данных.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as AxPath, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow};

const CODE: &str = "wqdzbfhmn83d23j4";
const UID: &str = "1";

/// ЗАПРЕЩЁННОЕ значение сущности: с ним `im.chat.get` молча отдаёт `null`.
const TRAP_ENTITY: &str = "TASKS";

/// ЕДИНСТВЕННОЕ верное значение сущности чата задачи.
const GOOD_ENTITY: &str = "TASKS_TASK";

/// ЗАПРЕЩЁННЫЙ ключ фильтра: с ним `tasks.task.list` молча отдаёт ноль строк.
const TRAP_FILTER_KEY: &str = "STATUS";

// ===========================================================================
// Стенд, который ВЕДЁТ СЕБЯ КАК ПОРТАЛ В ЛОВУШКАХ.
// ===========================================================================

/// Журнал вызовов и «ловушечное» поведение портала.
#[derive(Default)]
struct StubState {
    /// Метод → тела всех вызовов.
    calls: Mutex<Vec<(String, Value)>>,
    /// Задачи стенда.
    tasks: Mutex<HashMap<i64, Value>>,
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

    fn all(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

fn live_task(id: i64) -> Value {
    json!({
        "id": id.to_string(),
        "title": format!("Задача {id}"),
        "status": "2",
        "subStatus": "-1",
        "priority": "1",
        "createdBy": "1",
        "responsibleId": "1",
        "deadline": "2026-07-04T19:00:00+03:00",
        "groupId": "0",
        "group": [],
        "auditors": [],
        "accomplices": [],
        "chatId": 7,
        "newCommentsCount": 0,
        "action": {"start": true, "pause": false, "complete": true, "defer": true}
    })
}

async fn start_stub() -> (String, Arc<StubState>) {
    let state = Arc::new(StubState::default());
    state.tasks.lock().unwrap().insert(1, live_task(1));
    state.tasks.lock().unwrap().insert(3, live_task(3));

    let app = Router::new()
        .route("/rest/:uid/:code/:method", post(handle))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/rest/{UID}/{CODE}"), state)
}

fn ok(result: Value) -> Response {
    Json(json!({"result": result, "time": {"operating": 0.0}})).into_response()
}

async fn handle(
    State(state): State<Arc<StubState>>,
    AxPath((_uid, _code, method_json)): AxPath<(String, String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let method = method_json.trim_end_matches(".json").to_string();
    let params: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state
        .calls
        .lock()
        .unwrap()
        .push((method.clone(), params.clone()));

    match method.as_str() {
        // ЛОВУШКА 1. Живьём (проверено на test-work-2.bitrix24.ru):
        //   ENTITY_TYPE="TASKS"      → {"result":null}      HTTP 200
        //   ENTITY_TYPE="TASKS_TASK" → {"result":{"ID":7}}  HTTP 200
        "im.chat.get" => {
            if params["ENTITY_TYPE"].as_str() != Some(GOOD_ENTITY) {
                return ok(Value::Null);
            }
            let id = params["ENTITY_ID"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| params["ENTITY_ID"].as_i64())
                .unwrap_or_default();
            // Несуществующая задача — тоже `null`: у портала нет способа
            // отличить «сущности нет» от «у сущности нет чата», и обе формы
            // приезжают одинаково.
            match state.tasks.lock().unwrap().get(&id) {
                Some(_) => ok(json!({"ID": 7})),
                None => ok(Value::Null),
            }
        }
        // ЛОВУШКА 3. `STATUS` — псевдополе: ноль строк, успешный ответ.
        "tasks.task.list" => {
            if params["filter"].get(TRAP_FILTER_KEY).is_some() {
                return Json(json!({"result": {"tasks": []}, "total": 0})).into_response();
            }
            let all: Vec<Value> = state.tasks.lock().unwrap().values().cloned().collect();
            let total = all.len() as i64;
            let page: Vec<Value> = all
                .into_iter()
                // ЛОВУШКА 2 внутри списка.
                .map(|t| select_fields(&t, &params["select"]))
                .collect();
            Json(json!({"result": {"tasks": page}, "total": total})).into_response()
        }
        "tasks.task.get" => {
            let id = params["taskId"].as_i64().unwrap_or_default();
            let task = state.tasks.lock().unwrap().get(&id).cloned();
            match task {
                // ЛОВУШКА 2 и на карточке: `select` действует и здесь.
                Some(t) => ok(json!({"task": select_fields(&t, &params["select"])})),
                None => ok(json!([])),
            }
        }
        _ => ok(json!(true)),
    }
}

/// ЛОВУШКА 2 дословно: `select` фильтрует поля, а camelCase-имя портал МОЛЧА
/// игнорирует — поле просто выпадает из ответа и приезжает `null`.
///
/// Живой замер: `select:["ID","chatId","status"]` вернул
/// `{'chatId': None, 'status': None}`, а `["ID","CHAT_ID","STATUS"]` — те же
/// поля со значениями.
fn select_fields(task: &Value, select: &Value) -> Value {
    let Some(names) = select.as_array() else {
        return task.clone();
    };
    let mut out = serde_json::Map::new();
    out.insert("id".into(), task["id"].clone());
    for name in names.iter().filter_map(Value::as_str) {
        if name.chars().any(|c| c.is_ascii_lowercase()) {
            // camelCase — портал его не понимает и молчит.
            continue;
        }
        let camel = upper_snake_to_camel(name);
        if let Some(v) = task.get(&camel) {
            out.insert(camel, v.clone());
        }
    }
    Value::Object(out)
}

fn upper_snake_to_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for (i, ch) in name.chars().enumerate() {
        if ch == '_' {
            upper = true;
            continue;
        }
        if i == 0 || !upper {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch.to_ascii_uppercase());
            upper = false;
        }
    }
    out
}

async fn harness() -> (tempfile::TempDir, Arc<StubState>, Arc<B24State>, Router) {
    let (base_url, stub) = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        webhook: WebhookUrl::new(format!("https://p.bitrix24.ru/rest/{UID}/{CODE}/")),
        base_url_override: base_url,
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
            portal: "p.bitrix24.ru".into(),
        })
        .await;
    let app = forge_bitrix::routes::build_router(state.clone());
    (dir, stub, state, app)
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

// ===========================================================================
// СТРАЖ 1: ENTITY_TYPE
// ===========================================================================

/// **СТРАЖ ЛОВУШКИ 1.** `im.chat.get` вызывается ТОЛЬКО с
/// `ENTITY_TYPE = "TASKS_TASK"`.
///
/// Значение `"TASKS"` молча возвращает `{"result": null}` при HTTP 200 —
/// подтверждено живьём на тестовом портале. Ошибка неотличима от «у задачи нет
/// чата», поэтому проявляется как «у задач никогда не открывается лента» и
/// живёт месяцами.
///
/// Проверяется ТРИ вещи:
/// 1. в журнале стенда нет ни одного вызова с `"TASKS"`;
/// 2. каждый вызов несёт ровно `"TASKS_TASK"`;
/// 3. код не принимает `null` за валидный ответ.
#[tokio::test]
async fn the_entity_type_is_never_the_bare_tasks() {
    let (_d, stub, state, app) = harness().await;

    // Прогоняем все ветки, которые вообще могут дойти до `im.chat.get`.
    assert_eq!(send(&app, "GET", "/api/bitrix/tasks").await.0, StatusCode::OK);
    assert_eq!(send(&app, "GET", "/api/bitrix/tasks/1").await.0, StatusCode::OK);
    assert_eq!(
        send(&app, "POST", "/api/bitrix/tasks/1/start").await.0,
        StatusCode::OK
    );
    // И прямой вызов обёртки — единственный, который реально её зовёт.
    let client = state.api_client().await.unwrap();
    assert_eq!(client.task_chat_id(1).await.unwrap(), Some(7));

    let calls = stub.calls_of("im.chat.get");
    assert!(
        !calls.is_empty(),
        "страж бессмысленен, если метод не вызывался ни разу"
    );
    for (i, call) in calls.iter().enumerate() {
        let entity = call["ENTITY_TYPE"].as_str().unwrap_or("<нет>");
        assert_ne!(
            entity, TRAP_ENTITY,
            "вызов #{i}: ENTITY_TYPE=\"TASKS\" молча вернёт null — у задач не откроется лента"
        );
        assert_eq!(
            entity, GOOD_ENTITY,
            "вызов #{i}: единственное верное значение — TASKS_TASK"
        );
    }
}

/// Тот же страж со стороны ПОВЕДЕНИЯ: стенд отвечает `null` на неверную
/// сущность, и код обязан НЕ принять это за валидный чат.
///
/// Проверка на «отравленных» данных: если бы код брал идентификатор из `null`
/// как ноль, задача получила бы `dialog_id = "chat0"` — адрес, по которому
/// портал ответит отказом, а пользователь увидит «лента не грузится».
#[tokio::test]
async fn a_null_result_is_no_chat_and_not_chat_zero() {
    let (_d, _stub, state, _app) = harness().await;
    let client = state.api_client().await.unwrap();

    // Задачи 999 у стенда нет ⇒ `im.chat.get` отвечает `null`.
    assert_eq!(
        client.task_chat_id(999).await.unwrap(),
        None,
        "null обязан означать «чата нет», а не «чат с номером 0»"
    );
}

/// **СТРАЖ 1 ПО ИСХОДНИКАМ.** Ни одна строка `src/` не передаёт `"TASKS"` как
/// значение сущности.
///
/// Ловит регрессию в момент появления — даже если ветка ещё не вызывается ни
/// одним тестом. Комментарии пропускаются: они цитируют запрещённое значение
/// именно для того, чтобы объяснить запрет.
#[test]
fn no_source_line_passes_the_bare_tasks_entity() {
    let mut offenders = Vec::new();
    let mut entity_lines = 0usize;
    for path in sources() {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for (n, line) in production_lines(&text) {
            let code = line.trim_start();
            // Литерал ровно `"TASKS"` — с кавычками, чтобы не поймать
            // `"TASKS_TASK"` и `TASKS_TASK_UNREAD`.
            if code.contains("\"TASKS\"") {
                offenders.push(format!("{}:{}: {code}", path.display(), n + 1));
            }
            // Вторая, более узкая ловля: строка, которая ЯВНО передаёт
            // сущность, обязана нести только верное значение.
            if code.contains("ENTITY_TYPE") {
                entity_lines += 1;
                assert!(
                    !code.contains("\"TASKS\""),
                    "{}:{}: ENTITY_TYPE=\"TASKS\" — молчаливый null: {code}",
                    path.display(),
                    n + 1
                );
            }
        }
    }
    assert!(
        entity_lines > 0,
        "ни одна строка не передаёт ENTITY_TYPE — страж смотрел бы в пустоту"
    );
    assert!(
        offenders.is_empty(),
        "ENTITY_TYPE=\"TASKS\" молча возвращает null при HTTP 200 — \
         единственное верное значение TASKS_TASK:\n{}",
        offenders.join("\n")
    );
}

/// Строки БОЕВОГО кода файла: комментарии и модуль тестов отброшены.
///
/// Тесты отбрасываются намеренно, и это не послабление: слово `TASKS` живёт в
/// крейте ещё в одном, совершенно законном смысле — это ИМЯ ПРАВА вебхука
/// (`scope`), и юнит-тесты `has_task_scope` обязаны проверять его дословно.
/// Ловить их значило бы либо ослабить страж до бесполезности, либо переписать
/// чужие тесты ради формулировки. Комментарии отбрасываются потому, что
/// цитируют запрещённое значение ровно для объяснения запрета.
///
/// Модуль тестов в этом крейте всегда последний в файле (`#[cfg(test)] mod
/// tests` в самом низу), поэтому отсечения по первому вхождению достаточно.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let end = text
        .lines()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
        .unwrap_or(usize::MAX);
    text.lines()
        .enumerate()
        .take_while(|(n, _)| *n < end)
        .filter(|(_, l)| {
            let c = l.trim_start();
            !c.starts_with("//") && !c.starts_with('*')
        })
        .collect()
}

// ===========================================================================
// СТРАЖ 2: camelCase в select
// ===========================================================================

/// **СТРАЖ ЛОВУШКИ 2.** Карточка задачи читается БЕЗ `select`, а список — со
/// `select` из одних UPPERCASE-имён.
///
/// camelCase в `select` портал МОЛЧА игнорирует: `select:["ID","chatId"]` дал
/// `chatId: null`, а `["ID","CHAT_ID"]` — `chatId: 7`. Пустое значение при этом
/// неотличимо от настоящего `null`, поэтому ошибка выглядит как «у задач нет
/// чатов» и живёт месяцами.
#[tokio::test]
async fn the_select_never_carries_a_camel_case_name() {
    let (_d, stub, _state, app) = harness().await;

    assert_eq!(send(&app, "GET", "/api/bitrix/tasks").await.0, StatusCode::OK);
    assert_eq!(send(&app, "GET", "/api/bitrix/tasks/1").await.0, StatusCode::OK);

    // Карточка — БЕЗ `select` вовсе: угадывать полсотни имён в правильном
    // регистре дороже, чем не угадывать вовсе.
    let card_calls = stub.calls_of("tasks.task.get");
    assert!(!card_calls.is_empty(), "карточка не запрашивалась");
    for call in &card_calls {
        assert!(
            call.get("select").is_none(),
            "карточка обязана читаться БЕЗ select: {call}"
        );
    }

    // Список — со `select`, и в нём НИ ОДНОГО строчного символа.
    let list_calls = stub.calls_of("tasks.task.list");
    assert!(!list_calls.is_empty(), "список не запрашивался");
    for call in &list_calls {
        let select = call["select"]
            .as_array()
            .unwrap_or_else(|| panic!("список обязан просить поля явно: {call}"));
        assert!(!select.is_empty());
        for name in select.iter().filter_map(Value::as_str) {
            assert!(
                !name.chars().any(|c| c.is_ascii_lowercase()),
                "camelCase в select ({name}) портал МОЛЧА проигнорирует, \
                 и поле приедет null — неотличимо от настоящего null"
            );
        }
    }
}

/// Тот же страж со стороны РЕЗУЛЬТАТА: со `select` из UPPERCASE-имён поля
/// действительно доезжают.
///
/// Проверка на «отравленных» данных: стенд ведёт себя как портал и выбрасывает
/// camelCase-имена. Если бы плагин просил `chatId`, строка списка приехала бы
/// без чата — и лента задачи не открылась бы из директории `~/tasks` вовсе.
#[tokio::test]
async fn the_uppercase_select_actually_brings_the_fields() {
    let (_d, _stub, _state, app) = harness().await;
    let (status, v) = send(&app, "GET", "/api/bitrix/tasks").await;
    assert_eq!(status, StatusCode::OK);

    let rows = v["tasks"].as_array().expect("страница задач");
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row["chat_id"], 7, "CHAT_ID доехал: {row}");
        assert_eq!(row["dialog_id"], "chat7", "и превратился в DIALOG_ID");
        assert_eq!(row["status"], 2, "STATUS доехал");
        assert!(
            row["deadline"].is_string(),
            "DEADLINE доехал: {row}"
        );
    }
}

/// **СТРАЖ 2 ПО ИСХОДНИКАМ.** В таблице полей `select` нет camelCase-имён.
///
/// Таблица одна на весь крейт ([`forge_bitrix::tasks::LIST_SELECT`]), поэтому
/// проверить её можно прямо значением — без чтения файлов.
#[test]
fn the_frozen_select_table_is_uppercase_only() {
    let table = forge_bitrix::tasks::LIST_SELECT;
    assert!(!table.is_empty());
    for name in table {
        assert!(
            !name.chars().any(|c| c.is_ascii_lowercase()),
            "camelCase в LIST_SELECT: {name}"
        );
        assert!(
            name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()),
            "имя поля v1 состоит из UPPER_SNAKE: {name}"
        );
    }
    assert!(
        table.contains(&"CHAT_ID"),
        "без CHAT_ID лента задачи не откроется из списка"
    );
}

// ===========================================================================
// СТРАЖ 3: ключ STATUS в фильтре
// ===========================================================================

/// **СТРАЖ ЛОВУШКИ 3.** `tasks.task.list` НИКОГДА не вызывается с ключом
/// фильтра `"STATUS"`.
///
/// Живьём `{"RESPONSIBLE_ID":1,"STATUS":2,"GROUP_ID":0}` вернул `total: 0` при
/// двух полностью подходящих задачах — успехом, молча. `STATUS` у задач
/// псевдополе: оно пересекается с виртуальными состояниями (`subStatus`).
/// Работают только `REAL_STATUS` и операторные формы (`<STATUS`,
/// `<REAL_STATUS`).
///
/// Прогоняются ВСЕ пресеты фильтра: ошибка в одном из них дала бы пустую
/// директорию ровно на нём.
#[tokio::test]
async fn the_task_filter_never_carries_the_bare_status_key() {
    let (_d, stub, _state, app) = harness().await;

    for (preset, _) in forge_bitrix::routes::tasks::FILTERS {
        let (status, v) = send(&app, "GET", &format!("/api/bitrix/tasks?filter={preset}")).await;
        assert_eq!(status, StatusCode::OK, "пресет {preset}: {v}");
    }
    // И с уточнениями поверх пресета — они собираются другой веткой.
    assert_eq!(
        send(&app, "GET", "/api/bitrix/tasks?filter=active&responsible=1&group=0")
            .await
            .0,
        StatusCode::OK
    );

    let calls = stub.calls_of("tasks.task.list");
    assert!(calls.len() >= 6, "прогнаны не все ветки: {}", calls.len());
    for call in &calls {
        let Some(filter) = call.get("filter").and_then(Value::as_object) else {
            continue;
        };
        assert!(
            !filter.contains_key(TRAP_FILTER_KEY),
            "ключ STATUS молча даёт НОЛЬ записей при подходящих задачах: {call}"
        );
        // Состояние выражается только законными формами.
        for key in filter.keys() {
            if key.contains("STATUS") {
                assert!(
                    key == "REAL_STATUS" || key.starts_with('<') || key.starts_with('>'),
                    "непроверенная форма фильтра по статусу: {key}"
                );
            }
        }
    }
}

/// Тот же страж со стороны ТРАНСПОРТА: запрещённый ключ не доезжает до сети
/// вовсе — сборщик параметров отвергает его раньше.
///
/// Проверка на «отравленных» данных: подсовываем фильтр с `STATUS` напрямую в
/// обёртку. Ответ обязан быть отказом, а не пустым списком.
#[tokio::test]
async fn a_poisoned_filter_is_refused_before_the_network() {
    let (_d, stub, state, _app) = harness().await;
    let client = state.api_client().await.unwrap();

    let err = client
        .task_list(&json!({"RESPONSIBLE_ID": 1, "STATUS": 2}), 0)
        .await
        .expect_err("фильтр с ключом STATUS обязан быть отвергнут");
    assert!(
        err.to_string().contains("REAL_STATUS"),
        "текст отказа обязан подсказывать замену: {err}"
    );
    assert!(
        stub.calls_of("tasks.task.list").is_empty(),
        "запрещённый фильтр не имеет права доехать до портала"
    );

    // А законная форма проходит и возвращает задачи.
    let (rows, total) = client
        .task_list(&json!({"REAL_STATUS": 2}), 0)
        .await
        .expect("REAL_STATUS — рабочая форма");
    assert_eq!(rows.len(), 2);
    assert_eq!(total, Some(2));
}

/// СТРАЖ ПАДАЕТ НА НЕВЕРНОМ ВАРИАНТЕ: стенд ведёт себя как портал, и запрос с
/// `STATUS` действительно даёт ноль строк при двух подходящих задачах.
///
/// Без этой проверки страж доказывал бы только то, что наш код не делает
/// чего-то, — но не то, что это «что-то» и вправду опасно.
#[tokio::test]
async fn the_forbidden_filter_really_returns_nothing() {
    let (_d, _stub, state, _app) = harness().await;
    let client = state.api_client().await.unwrap();

    // Законный фильтр — две задачи.
    let (good, _) = client.task_list(&json!({"REAL_STATUS": 2}), 0).await.unwrap();
    assert_eq!(good.len(), 2);

    // Тот же смысл через запрещённый ключ — НОЛЬ строк и успешный ответ.
    // Обёртку обойти нельзя, поэтому зовём портал напрямую.
    let raw = client
        .call_value(
            "tasks.task.list",
            json!({"filter": {"STATUS": 2}, "select": ["ID"]}),
        )
        .await
        .expect("портал отвечает УСПЕХОМ, а не ошибкой");
    assert_eq!(
        raw.result["tasks"].as_array().map(Vec::len),
        Some(0),
        "ровно это и делает ловушку опасной: успех с пустотой"
    );
    assert_eq!(raw.total, Some(0));
}

// ===========================================================================
// Общее: ни один страж не должен молчать из-за пустого журнала.
// ===========================================================================

/// Журнал стенда не пуст ни по одному из трёх методов — иначе стражи выше
/// проверяли бы пустое множество и были бы зелёными всегда.
#[tokio::test]
async fn every_guard_has_something_to_look_at() {
    let (_d, stub, state, app) = harness().await;
    assert_eq!(send(&app, "GET", "/api/bitrix/tasks").await.0, StatusCode::OK);
    assert_eq!(send(&app, "GET", "/api/bitrix/tasks/1").await.0, StatusCode::OK);
    let client = state.api_client().await.unwrap();
    let _ = client.task_chat_id(1).await.unwrap();

    let methods: Vec<String> = stub.all().into_iter().map(|(m, _)| m).collect();
    for required in ["tasks.task.list", "tasks.task.get", "im.chat.get"] {
        assert!(
            methods.iter().any(|m| m == required),
            "страж по {required} смотрел бы в пустой журнал; вызванные методы: {methods:?}"
        );
    }
}

// ===========================================================================
// Обход исходников.
// ===========================================================================

/// Все `.rs` крейта.
fn sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    collect_rs(&root, &mut out);
    assert!(
        out.len() > 5,
        "исходники не найдены: {} — страж по коду молчал бы всегда",
        root.display()
    );
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
