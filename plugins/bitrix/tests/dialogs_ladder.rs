//! ТЕСТ-СТРАЖ ЛЕСТНИЦЫ ПРОВЕРОК data-роутов: `409 → 503 → 404 → 400`.
//!
//! # Что охраняется
//!
//! Порядок проверок в роутах данных. Он выглядит как вопрос вкуса ровно до
//! того момента, пока не окажется нарушен: роут, отвечающий `400` раньше
//! `409`, превращается в **оракул валидности** — неподключённый клиент по коду
//! ответа узнаёт, какие идентификаторы и параметры портал считает
//! осмысленными, и прогоняет через наш парсер что угодно.
//!
//! # Почему это нельзя проверить компилятором
//!
//! Экстракторы axum (`Query<T>`, `Path<T>`, `Json<T>`) отрабатывают ДО тела
//! хендлера. Замена ручного разбора query на типизированный `Query<Params>`
//! компилируется, читается лучше — и молча ломает лестницу: `?limit=abc`
//! начинает давать `400` раньше, чем проверка подключения успевает сказать
//! `409`. Ни одна другая проверка репозитория этого не заметит.
//!
//! Поэтому страж бьёт именно по КОДАМ ОТВЕТА, а не по коду программы.
//!
//! # Состояние роутов по фазам
//!
//! Фаза 4 дала `/api/bitrix/filters` и `/api/bitrix/dialogs` — оба без
//! сегментов пути, то есть первые две ступени. Фаза 5 добавила
//! `/api/bitrix/dialogs/:id/messages` и `/api/bitrix/messages/:mid`, а вместе
//! с ними — ступень `404`: враждебный `:id` обязан получить «такого диалога
//! нет» ПОСЛЕ авторизации и связи, но ДО разбора `?limit`.
//!
//! # Почему `404`, а не `400`, на неразбираемый `:id`
//!
//! Снаружи «идентификатор не разбирается» и «такого диалога у портала нет» —
//! одно и то же: пользователь не может открыть этот диалог. Разные коды
//! означали бы, что по ответу перебирается ФОРМА валидного идентификатора
//! портала, а вместе с ней и то, какие диалоги существуют.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::login::{B24UserDto, LoginFlow};
use forge_bitrix::state::B24State;

/// Заведомо враждебные значения параметров.
///
/// Всё, что здесь есть, приходило в реальные веб-панели: длинные строки
/// (переполнение буфера в чужом коде), юникод-мусор и невидимые символы
/// (обход сравнений), `../..` и нулевой байт в процентной кодировке (обход
/// путей), числа за границами `i64` (паника при разборе).
const HOSTILE: [&str; 14] = [
    "..%2F..%2F..%2Fetc%2Fpasswd",
    "%00",
    "19%00",
    "chat7%00.json",
    "-9999999999999999999999999",
    "99999999999999999999999999999999",
    "0x7fffffffffffffff",
    "NaN",
    "%E2%80%8Bchat7",
    "%D1%87%D0%B0%D1%82",
    "chat%EF%BC%97",
    "%7Bjson%3Atrue%7D",
    "abc",
    "%3Cscript%3Ealert(1)%3C%2Fscript%3E",
];

/// Роутер плагина на изолированном конфиге.
fn app_with(f: impl FnOnce(&mut BitrixConfig)) -> (tempfile::TempDir, Arc<B24State>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = BitrixConfig {
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        // Порт 1 закрыт всегда: если запрос всё же дойдёт до сети, тест
        // упадёт по таймауту вместо того, чтобы молча проехать.
        base_url_override: "http://127.0.0.1:1/rest/1/stub".into(),
        connect_timeout_secs: 1,
        request_timeout_secs: 1,
        ..BitrixConfig::default()
    };
    f(&mut cfg);
    let state = B24State::new(cfg);
    let app = forge_bitrix::routes::build_router(state.clone());
    (dir, state, app)
}

/// Плагин, прошедший первые две ступени: вебхук принят, связь считается живой.
///
/// Порт в `base_url_override` закрыт всегда, поэтому запрос, дошедший до сети,
/// упирается в таймаут и отвечает `503` — по этому и видно, что ступени
/// пройдены, а не пропущены.
async fn connected_app() -> (tempfile::TempDir, Arc<B24State>, Router) {
    let (dir, state, app) = app_with(|c| {
        c.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
    });
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "p.bitrix24.ru".into(),
        })
        .await;
    state.note_ok(chrono::Utc::now().timestamp()).await;
    (dir, state, app)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 512 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Все враждебные адреса, которые умеет собрать панель.
fn hostile_uris() -> Vec<String> {
    let mut out = Vec::new();
    for value in HOSTILE {
        out.push(format!("/api/bitrix/dialogs?limit={value}"));
        out.push(format!("/api/bitrix/dialogs?offset={value}"));
        out.push(format!("/api/bitrix/dialogs?filter={value}"));
        out.push(format!("/api/bitrix/dialogs?dialog_id={value}"));
        out.push(format!("/api/bitrix/filters?{value}"));
        // Ф5: те же значения в СЕГМЕНТЕ ПУТИ — их разбирает другой код
        // (`require_dialog_id`), и он тоже обязан молчать до авторизации.
        out.push(format!("/api/bitrix/dialogs/{value}/messages"));
        out.push(format!("/api/bitrix/dialogs/chat27/messages?limit={value}"));
        out.push(format!("/api/bitrix/dialogs/chat27/messages?before_id={value}"));
        out.push(format!("/api/bitrix/dialogs/chat27/messages?after_id={value}"));
        out.push(format!("/api/bitrix/messages/{value}"));
        out.push(format!("/api/bitrix/messages/113?dialog_id={value}"));
    }
    // Дублирующиеся ключи, пустые значения, мусор без ключа и очень длинная
    // строка — отдельными случаями: их разбирает другой код.
    out.push("/api/bitrix/dialogs?limit=1&limit=abc&offset=-1&filter=".into());
    out.push("/api/bitrix/dialogs?limit&offset&filter".into());
    out.push("/api/bitrix/dialogs?=&=&=".into());
    out.push(format!("/api/bitrix/dialogs?limit={}", "9".repeat(4096)));
    out.push(format!("/api/bitrix/dialogs?filter={}", "a".repeat(8192)));
    out
}

/// ГЛАВНЫЙ СТРАЖ: без подключения ЛЮБОЙ враждебный ввод получает `409`, а не
/// `400` и не `500`.
///
/// Падает, если ручной разбор query заменить на `Query<T>`-экстрактор:
/// `?limit=abc` тогда начнёт отвечать `400`.
#[tokio::test]
async fn hostile_dialog_ids_hit_auth_before_any_parsing() {
    let (_dir, _state, app) = app_with(|_| {});

    for uri in hostile_uris() {
        let (status, body) = get(&app, &uri).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{uri} → {status}, а обязан быть 409 (авторизация раньше разбора)\n{body}"
        );
        assert_eq!(
            body["error"]["code"], "not_authorized",
            "{uri}: код ошибки — контракт с фронтом"
        );
        // Ответ не должен цитировать враждебную строку обратно: это и лишний
        // рефлектор, и подсказка о том, что именно мы разбираем.
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("script"), "{uri}: эхо ввода в ответе {text}");
    }
}

/// ВТОРАЯ СТУПЕНЬ: подключение есть, связи нет — `503`, и по-прежнему НЕ `400`.
///
/// Порядок именно такой, потому что 503 повторяем (панель показывает
/// «переподключение»), а 400 требует правки запроса — сообщать «почините
/// запрос», когда сломана сеть, значит отправить пользователя чинить не то.
#[tokio::test]
async fn a_broken_connection_answers_before_the_parser() {
    let (_dir, state, app) = app_with(|c| {
        c.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
    });
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "p.bitrix24.ru".into(),
        })
        .await;
    state
        .note_down(chrono::Utc::now().timestamp(), "connect timeout")
        .await;

    for uri in [
        "/api/bitrix/dialogs?limit=abc",
        "/api/bitrix/dialogs?offset=%00",
        "/api/bitrix/filters",
        // Ф5: неразбираемый `:id` тоже ждёт своей очереди — связь важнее.
        "/api/bitrix/dialogs/%00/messages?limit=abc",
        "/api/bitrix/messages/NaN",
    ] {
        let (status, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}\n{body}");
        assert_eq!(body["error"]["code"], "not_connected");
    }
}

/// ТРЕТЬЯ СТУПЕНЬ (Ф5): подключение и связь есть — теперь неразбираемый `:id`
/// даёт `404`, и по-прежнему НЕ `400`.
///
/// Ступень появилась вместе с историей сообщений: до неё роутов с сегментом
/// пути не было вовсе. Проверяется и то, что 404 наступает РАНЬШЕ разбора
/// query: `?limit=abc` в том же запросе не превращает ответ в 400.
#[tokio::test]
async fn an_unparseable_dialog_id_answers_404_before_the_parser() {
    let (_dir, state, app) = connected_app().await;

    for uri in [
        "/api/bitrix/dialogs/%D0%BC%D1%83%D1%81%D0%BE%D1%80/messages",
        "/api/bitrix/dialogs/CHAT7/messages",
        "/api/bitrix/dialogs/chat7a/messages?limit=abc",
        "/api/bitrix/dialogs/+19/messages",
        "/api/bitrix/dialogs/..%2F..%2Fetc%2Fpasswd/messages",
        "/api/bitrix/messages/113?dialog_id=%3Cscript%3E",
        // `dialog_id` не передан вовсе — сообщение адресовать нечем.
        "/api/bitrix/messages/113",
    ] {
        let (status, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}\n{body}");
        assert_eq!(body["error"]["code"], "unknown_dialog", "{uri}");
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("script"), "{uri}: эхо ввода в ответе {text}");
    }

    // Нечисловой `:mid` — «сообщения нет», и тоже до разбора query.
    for uri in [
        "/api/bitrix/messages/abc?dialog_id=chat27",
        "/api/bitrix/messages/0?dialog_id=chat27",
        "/api/bitrix/messages/-5?dialog_id=chat27&limit=abc",
    ] {
        let (status, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}\n{body}");
        assert_eq!(body["error"]["code"], "unknown_message", "{uri}");
    }

    // …а РАЗБИРАЕМЫЙ `:id` ступень проходит и упирается в закрытый порт 1,
    // то есть в сеть: 404 выше — про идентификатор, а не про роут.
    let (status, _) = get(&app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_ne!(status, StatusCode::NOT_FOUND);
    drop(state);
}

/// ПОСЛЕДНЯЯ СТУПЕНЬ ленты: `:id` разобрался — и только теперь мусор в query
/// даёт `400`.
#[tokio::test]
async fn garbage_history_parameters_are_rejected_after_the_dialog_resolves() {
    let (_dir, _state, app) = connected_app().await;

    for (uri, param) in [
        ("/api/bitrix/dialogs/chat27/messages?limit=abc", "`limit`"),
        ("/api/bitrix/dialogs/chat27/messages?before_id=x", "`before_id`"),
        ("/api/bitrix/dialogs/chat27/messages?after_id=1.5", "`after_id`"),
    ] {
        let (status, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}\n{body}");
        assert_eq!(body["error"]["code"], "invalid_input");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(param),
            "сообщение обязано называть параметр: {body}"
        );
    }
}

/// ПОСЛЕДНЯЯ СТУПЕНЬ: всё в порядке — и только теперь мусор в параметрах даёт
/// `400`.
///
/// Тест доказывает, что `400` вообще достижим: без него страж выше проходил бы
/// и на роуте, который не разбирает параметры совсем.
#[tokio::test]
async fn garbage_parameters_are_rejected_once_the_ladder_is_passed() {
    let (_dir, _state, app) = connected_app().await;

    let (status, body) = get(&app, "/api/bitrix/dialogs?limit=abc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_input");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("`limit`"),
        "сообщение обязано называть параметр: {body}"
    );

    // …а незнакомая директория — НЕ ошибка: значение приходит из настроек
    // фронта, которые браузер держит в кэше PWA неделями. Запрос уходит в сеть
    // (порт 1 закрыт) и получает 503 — то есть разбор параметров он ПРОШЁЛ.
    let (status, body) = get(&app, "/api/bitrix/dialogs?filter=выдумка").await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Роуты данных смонтированы: 409 — это «плагин есть, портал не подключён», а
/// 404 — «плагина нет вовсе», и фронт по этой разнице гейтит кнопку вкладки.
#[tokio::test]
async fn the_data_routes_exist_even_without_a_webhook() {
    let (_dir, _state, app) = app_with(|_| {});
    for uri in [
        "/api/bitrix/dialogs",
        "/api/bitrix/filters",
        "/api/bitrix/dialogs/chat7/messages",
        "/api/bitrix/messages/113?dialog_id=chat7",
        // Ф10: задачи стоят на той же лестнице. Мусорный `:id` тоже даёт 409,
        // а не 404: авторизация проверяется РАНЬШЕ разбора идентификатора.
        "/api/bitrix/tasks",
        "/api/bitrix/tasks/3",
        "/api/bitrix/tasks/counters",
        "/api/bitrix/tasks/мусор",
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}");
    }
}


/// Те же враждебные `:id` НА РОУТАХ АНАЛИЗА ЧАТА — со своими методами и с
/// битым телом заодно.
///
/// Отдельным тестом, а не строкой в [`hostile_uris`], по двум причинам: у
/// четырёх из семи путей метод не `GET` (иначе `405` пришёл бы раньше
/// лестницы и ничего не проверил), и половина из них принимает ТЕЛО — а
/// именно ради него тело объявлено `Option<Json<T>>`. Экстрактор `Json<T>`
/// отверг бы `{not json` с кодом `400` ДО проверки авторизации и превратил бы
/// роут в оракул валидности идентификаторов портала.
#[tokio::test]
async fn hostile_ids_on_the_analysis_routes_hit_auth_first() {
    let (_dir, _state, app) = app_with(|_| {});

    for value in HOSTILE {
        for (method, tail, body) in [
            ("GET", "prompt", None),
            ("PUT", "prompt", Some("{not json")),
            ("GET", "analysis", None),
            ("POST", "analysis", Some("{not json")),
            ("DELETE", "analysis", None),
            ("DELETE", "analysis/thread", None),
            ("POST", "analysis/cancel", Some("{not json")),
        ] {
            let uri = format!("/api/bitrix/dialogs/{value}/{tail}");
            let req = axum::http::Request::builder().method(method).uri(&uri);
            let req = match body {
                Some(b) => req
                    .header("content-type", "application/json")
                    .body(Body::from(b))
                    .unwrap(),
                None => req.body(Body::empty()).unwrap(),
            };
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 512 * 1024)
                .await
                .unwrap();
            let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "{method} {uri} → {status}, а обязан быть 409\n{json}"
            );
            assert_eq!(json["error"]["code"], "not_authorized", "{method} {uri}");
            let text = serde_json::to_string(&json).unwrap();
            assert!(!text.contains("script"), "{method} {uri}: эхо ввода {text}");
        }
    }
}

/// ФИКСАЦИЯ ДВУХ ЛЕСТНИЦ: локальные роуты анализа отвечают `200` БЕЗ связи с
/// порталом, а запускающие — `503`.
///
/// Это не мелочь оформления, а функциональное требование: разбор и заметка
/// лежат в локальном файле, и прятать их ровно тогда, когда портал отвалился,
/// значило бы убирать контекст в момент, когда он полезнее всего. Добавленный
/// когда-нибудь `require_ready` на `/prompt` покрасит этот тест.
#[tokio::test]
async fn the_analysis_routes_stand_on_two_different_ladders() {
    // Вебхук принят, но связь ПАДАЮЩАЯ: `note_down` внутри окна грейса даёт
    // ровно то состояние, в котором пользователь читает историю из кэша.
    let (_dir, state, app) = app_with(|c| {
        c.webhook = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
    });
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "p.bitrix24.ru".into(),
        })
        .await;
    state
        .note_down(chrono::Utc::now().timestamp(), "network is down")
        .await;

    // Локальный файл — 200 при мёртвой связи.
    let (status, json) = get(&app, "/api/bitrix/dialogs/chat7/prompt").await;
    assert_eq!(status, StatusCode::OK, "заметка не требует портала: {json}");
    assert_eq!(json["prompt"], "");
    assert!(json["updated_at"].is_null());

    let (status, json) = get(&app, "/api/bitrix/dialogs/chat7/analysis").await;
    assert_eq!(status, StatusCode::OK, "разбор не требует портала: {json}");
    assert_eq!(json["text"], "");
    assert!(
        json["thread"].as_array().unwrap().is_empty(),
        "тред всегда массив"
    );

    // А запуск и отмена — требуют: без портала истории не существует.
    for (method, tail) in [("POST", "analysis"), ("POST", "analysis/cancel")] {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(format!("/api/bitrix/dialogs/chat7/{tail}"))
            .header("content-type", "application/json")
            .body(Body::from("{\"kind\":\"analyze\"}"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {tail}"
        );
    }
}

/// Полный круг по локальным роутам: заметка, разбор и тред — ТРИ независимые
/// величины, и очистка каждой не трогает две другие.
#[tokio::test]
async fn the_three_stored_values_are_independent() {
    let (_dir, state, app) = connected_app().await;
    let key = "chat7";

    // Заметка через роут, разбор и тред — напрямую в хранилище (роут запуска
    // потребовал бы живого портала).
    let put = axum::http::Request::builder()
        .method("PUT")
        .uri("/api/bitrix/dialogs/chat7/prompt")
        .header("content-type", "application/json")
        .body(Body::from("{\"prompt\":\"  Это заказчик\\r\\n \"}"))
        .unwrap();
    let resp = app.clone().oneshot(put).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["prompt"], "Это заказчик", "сохранено обрезанным");
    assert!(json["updated_at"].as_i64().unwrap() > 0);

    {
        let mut guard = state.prompts.lock().await;
        guard.set_analysis(key, "РАЗБОР", 7042, 137, 1_800_000_000);
        guard.push_turn(key, "вопрос", "ответ", 1_800_000_001);
        forge_bitrix::analysis::prompts::save(&state.config.prompts_path, &guard).unwrap();
    }

    let (_, json) = get(&app, "/api/bitrix/dialogs/chat7/analysis").await;
    assert_eq!(json["text"], "РАЗБОР");
    assert_eq!(json["last_message_id"], 7042);
    assert_eq!(json["messages_used"], 137);
    assert_eq!(json["thread"][0]["q"], "вопрос");

    // Стираем ТРЕД — разбор и заметка целы.
    let del = |uri: &str| {
        let uri = uri.to_string();
        let app = app.clone();
        async move {
            let resp = app
                .oneshot(
                    axum::http::Request::builder()
                        .method("DELETE")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap();
            let json: Value = serde_json::from_slice(&bytes).unwrap();
            (status, json)
        }
    };

    let (status, json) = del("/api/bitrix/dialogs/chat7/analysis/thread").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cleared"], true);
    let (_, json) = get(&app, "/api/bitrix/dialogs/chat7/analysis").await;
    assert_eq!(json["text"], "РАЗБОР", "разбор не тронут");
    assert!(json["thread"].as_array().unwrap().is_empty());

    // Стираем РАЗБОР — заметка по-прежнему цела.
    let (status, json) = del("/api/bitrix/dialogs/chat7/analysis").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["cleared"], true);
    let (_, json) = get(&app, "/api/bitrix/dialogs/chat7/prompt").await;
    assert_eq!(json["prompt"], "Это заказчик", "заметка пережила обе очистки");

    // Повторное нажатие — НЕ ошибка: кнопку жмут из двух вкладок сразу.
    for uri in [
        "/api/bitrix/dialogs/chat7/analysis",
        "/api/bitrix/dialogs/chat7/analysis/thread",
    ] {
        let (status, json) = del(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(json["cleared"], false, "{uri}");
    }
}
