//! Integration smoke-test каркаса плагина Telegram.
//!
//! Поднимает роутер плагина в текущем процессе (`forge_telegram::init` +
//! `register_routes`) на реальном listener'е `127.0.0.1:0` и проверяет
//! контракт, на который опирается фронтенд:
//!
//! **Состояния логина.** Без сети достижимы три: `not_configured`
//! (кредов нет), `disconnected` (креды есть, сессии нет) и `authorized`
//! без runtime (сессия есть, MTProto-соединения нет — через `set_login`).
//! Покрыты `test_status_is_not_configured`,
//! `test_status_is_disconnected_with_credentials` и
//! `test_status_authorized_without_connection` (data-роуты в нём отвечают
//! `503 not_connected`, а не выкидывают на форму логина).
//!
//! **REST.** `test_routes_report_expected_error_codes` держит таблицу
//! «метод × путь × тело → HTTP-статус + `code`» для логина, диалогов,
//! сообщений, read/typing и медиа. Загрузка файла проверяется отдельно:
//! `test_upload_rejects_before_reading_the_body` шлёт сырой HTTP с
//! `Content-Length: 64 МиБ` и десятком реальных байт — ответ 409 обязан
//! прийти ДО тела; `test_upload_without_auth_returns_contract_body`
//! повторяет это обычным клиентом. Статусы 411/413/400 у загрузки без
//! авторизации недостижимы по построению (авторизация проверяется первой),
//! поэтому unit-тесты живут в `routes/upload.rs`, а по проводу — в
//! authorized-состоянии: `test_upload_chunked_without_content_length_is_411`,
//! `test_upload_over_limit_is_413_before_reading_the_body` и happy-path
//! `test_upload_stages_unicode_name_end_to_end`.
//!
//! **Кнопки ботов.** `test_callback_route_is_mounted_as_post_only` —
//! двухпараметрический `POST /dialogs/:id/messages/:mid/callback` реально
//! примонтирован (405 на GET/PUT/DELETE вместо 404) и не перехватывает
//! соседний `POST /messages`; `test_callback_without_connection_is_not_connected`
//! держит порядок проверок «авторизация → соединение → разбор тела».
//!
//! **WS.** `test_ws_ping_pong` (первый кадр `login_state`, ping/pong),
//! `test_ws_receives_broadcast_without_filtering`,
//! `test_ws_delivers_message_event_shapes` (форма кадров = контракт фронта),
//! `test_ws_closes_idle_connection_without_pong` и
//! `test_ws_keeps_alive_while_client_ponging` — последние два поднимают стенд
//! с укороченными таймингами (`TelegramState::with_ws_timings`), иначе тест
//! ждал бы боевую минуту. Краевые случаи провода:
//! `test_ws_lagged_client_gets_resync_and_survives` (переполнение broadcast),
//! `test_ws_survives_garbage_binary_and_huge_frames` (не-JSON кадры и
//! сброс idle-таймера ими), `test_ws_shutdown_closes_client_promptly`,
//! `test_ws_upgrade_tolerates_token_and_unknown_query`,
//! `test_ws_broadcast_reaches_multiple_clients`.
//!
//! **Opt-in.** `test_routes_absent_without_the_plugin` собирает роутер БЕЗ
//! `register_routes` и убеждается, что весь префикс `/api/telegram/*` и
//! `/ws/telegram` отвечают 404 — главный инвариант фичи.
//!
//! **Прочее.** `test_logout_succeeds_and_status_follows`,
//! `test_workers_start_and_stop_cleanly`, `test_config_load_with_temp_paths`.
//!
//! Тесты НЕ требуют сети и не создают grammers-клиента. Пути конфига
//! переопределяются через `FORGE_TELEGRAM_*` либо задаются прямо в
//! [`TelegramConfig`], поэтому домашний каталог пользователя не
//! затрагивается.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use forge_telegram::config::TelegramConfig;
use forge_telegram::state::TelegramState;
use forge_telegram::ws::protocol::TgServerMsg;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Живой сервер с примонтированным плагином.
struct Harness {
    addr: SocketAddr,
    state: Arc<TelegramState>,
    _tempdir: tempfile::TempDir,
    _serve: tokio::task::JoinHandle<()>,
}

/// Поднимает плагин на свободном порту. Конфиг полностью изолирован во
/// временном каталоге (никаких обращений к `~/.config/forge`).
async fn start_harness() -> Harness {
    start_harness_with(false, None).await
}

/// Тот же стенд, но с кредами приложения: состояние логина стартует как
/// `disconnected` (креды есть, сессии нет) вместо `not_configured`.
async fn start_harness_configured() -> Harness {
    start_harness_with(true, None).await
}

/// Общий конструктор стенда.
///
/// * `with_credentials` — класть ли в конфиг `api_id`/`api_hash`. Это
///   единственный способ достичь состояния `disconnected` без сети: реального
///   MTProto-подключения тест не делает.
/// * `ws_timings` — укороченные heartbeat/idle для проверки закрытия по
///   таймауту (боевые 15с/60с превратили бы тест в минутный).
async fn start_harness_with(
    with_credentials: bool,
    ws_timings: Option<(Duration, Duration)>,
) -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let mut cfg = TelegramConfig {
        session_path: tempdir.path().join("telegram.session"),
        cache_dir: tempdir.path().join("cache"),
        ..TelegramConfig::default()
    };
    if with_credentials {
        cfg.api_id = Some(1);
        cfg.api_hash = Some("0123456789abcdef0123456789abcdef".into());
    }

    let state = match ws_timings {
        // `init` сам создаёт состояние боевым конструктором, поэтому для
        // укороченных таймингов собираем состояние явно. Валидация конфига
        // при этом не теряется — она вызывается тут же.
        Some((hb, idle)) => {
            cfg.validate().expect("конфиг стенда обязан быть валидным");
            TelegramState::with_ws_timings(cfg, hb, idle)
        }
        None => forge_telegram::init(cfg).expect("init must succeed on a valid config"),
    };
    let app: Router = forge_telegram::register_routes(Router::new(), state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    Harness {
        addr,
        state,
        _tempdir: tempdir,
        _serve: serve,
    }
}

#[tokio::test]
async fn test_status_is_not_configured() {
    let h = start_harness().await;
    let resp = reqwest::get(format!("http://{}/api/telegram/status", h.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], true);
    assert_eq!(body["state"], "not_configured");
    assert!(body["user"].is_null());
    assert!(body["error"].is_null());

    forge_telegram::shutdown(&h.state).await;
}

/// Контракт ошибок в состоянии `not_configured`.
///
/// С Фазы 2 login-роуты РЕАЛЬНЫЕ, поэтому безусловной 409 больше нет —
/// каждый отвечает по существу:
///
/// - пустое тело шага логина ⇒ `400 invalid_input` (нет обязательного поля);
/// - заполненное тело без кредов ⇒ `409 invalid_state` (сначала api_id/api_hash);
/// - data-роуты ⇒ `409 not_authorized` (аккаунт не привязан).
#[tokio::test]
async fn test_routes_report_expected_error_codes() {
    let h = start_harness().await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();

    // (метод, путь, тело, ожидаемый статус, ожидаемый код)
    let cases: &[(&str, &str, Value, u16, &str)] = &[
        (
            "POST",
            "/api/telegram/login/start",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "POST",
            "/api/telegram/login/code",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "POST",
            "/api/telegram/login/password",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "POST",
            "/api/telegram/login/start",
            json!({"phone": "+79990000000"}),
            409,
            "invalid_state",
        ),
        (
            "POST",
            "/api/telegram/login/code",
            json!({"code": "12345"}),
            409,
            "invalid_state",
        ),
        (
            "POST",
            "/api/telegram/login/password",
            json!({"password": "pw"}),
            409,
            "invalid_state",
        ),
        (
            "GET",
            "/api/telegram/dialogs",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/telegram/dialogs/123/messages",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/telegram/dialogs/123/messages",
            json!({}),
            409,
            "not_authorized",
        ),
        // Одно сообщение по id: фронт зовёт его, когда пользователь кликнул
        // по цитате, а оригинала нет в загруженной истории. Лестница та же —
        // `:mid` разбирается ПОСЛЕ авторизации.
        (
            "GET",
            "/api/telegram/dialogs/123/messages/7",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/telegram/dialogs/123/messages/not-a-number",
            json!({}),
            409,
            "not_authorized",
        ),
        // Нажатие inline-кнопки бота. Авторизация проверяется ДО разбора
        // `:mid` и тела, поэтому пустое тело здесь даёт 409, а не 400.
        (
            "POST",
            "/api/telegram/dialogs/123/messages/7/callback",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/telegram/dialogs/123/messages/7/callback",
            json!({"data": "YQ==", "game": false}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/telegram/dialogs/123/read",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/telegram/dialogs/123/typing",
            json!({}),
            409,
            "not_authorized",
        ),
        // Медиа: та же лестница. Авторизация проверяется ДО обращения к
        // диску и до Telegram, поэтому неавторизованный запрос обязан
        // получить 409, а не 404/503.
        (
            "GET",
            "/api/telegram/media/123/7",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/telegram/media/123/7?kind=thumb",
            json!({}),
            409,
            "not_authorized",
        ),
        // А вот форму параметров роут проверяет раньше авторизации: мусор в
        // пути — ошибка клиента в любом состоянии логина.
        (
            "GET",
            "/api/telegram/media/not-a-number/7",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "GET",
            "/api/telegram/media/123/7?kind=thmub",
            json!({}),
            400,
            "invalid_input",
        ),
    ];

    for (method, path, body, status, code) in cases {
        let url = format!("{base}{path}");
        let req = match *method {
            "GET" => client.get(&url),
            _ => client.post(&url).json(body),
        };
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), *status, "{method} {path} {body}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(
            got["error"]["code"].as_str().unwrap_or_default(),
            *code,
            "{method} {path} {body}"
        );
        assert!(
            !got["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{method} {path}: message обязателен"
        );
        assert!(
            got["error"]["retry_after_secs"].is_null(),
            "{method} {path}: retry_after_secs только у flood_wait"
        );
    }

    forge_telegram::shutdown(&h.state).await;
}

/// `POST /logout` успешен из любого состояния и оставляет статус
/// «не авторизован» — на это опирается кнопка выхода в UI.
#[tokio::test]
async fn test_logout_succeeds_and_status_follows() {
    let h = start_harness().await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/telegram/logout"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["state"], "not_configured");

    let body: Value = reqwest::get(format!("{base}/api/telegram/status"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["state"], "not_configured");

    forge_telegram::shutdown(&h.state).await;
}

#[tokio::test]
async fn test_ws_ping_pong() {
    let h = start_harness().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .expect("WS-апгрейд должен проходить");

    // Первый кадр — снапшот состояния логина.
    let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("первый кадр не пришёл вовремя")
        .expect("поток закрылся")
        .expect("ошибка чтения кадра");
    let text = first.into_text().unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "login_state");
    assert_eq!(v["state"], "not_configured");

    // Сервер шлёт ping раз в 15с; чтобы не ждать, эмулируем его через
    // broadcast — путь кадра до клиента тот же самый.
    h.state.broadcast_event(TgServerMsg::Ping);
    let ping = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("ping не пришёл")
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(&ping.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "ping");

    // Отвечаем pong — соединение обязано остаться живым.
    ws.send(Message::Text(r#"{"type":"pong"}"#.to_string()))
        .await
        .unwrap();

    h.state.broadcast_event(TgServerMsg::Resync);
    let after = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("после pong соединение должно продолжать работать")
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(&after.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "resync");

    forge_telegram::shutdown(&h.state).await;
}

#[tokio::test]
async fn test_ws_receives_broadcast_without_filtering() {
    let h = start_harness().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();

    // Пропускаем снапшот login_state.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();

    // Событие про произвольный диалог: фильтрации по активному диалогу нет,
    // поэтому подписчик обязан его получить (на этом держится unread-бейдж).
    h.state.broadcast_event(TgServerMsg::Read {
        dialog_id: "-1001234567890123".into(),
        max_id: 42,
        direction: "in".into(),
    });
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("событие не доехало")
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "read");
    assert_eq!(v["dialog_id"], "-1001234567890123");
    assert_eq!(v["max_id"], 42);

    forge_telegram::shutdown(&h.state).await;
}

/// Кадры realtime-событий доезжают до подписчика в той форме, на которую
/// написан фронт (`buildHandlers` в `js/telegram/main.js`).
///
/// Проверяется именно ПРОВОД: `type` в snake_case, 64-битные id — строками,
/// `message_id` — числом, Option-поля присутствуют как `null`.
#[tokio::test]
async fn test_ws_delivers_message_event_shapes() {
    let h = start_harness().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    // Снапшот login_state.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();

    let message = forge_telegram::dto::MessageDto {
        id: 4242,
        dialog_id: "-1001234567890".into(),
        out: false,
        date: 1_800_000_000,
        text: "мяу".into(),
        ..forge_telegram::dto::MessageDto::default()
    };
    let events = vec![
        TgServerMsg::NewMessage {
            message: message.clone(),
            // Флаги диалога едут на уровне кадра: решение «звенеть ли»
            // принимает сервер (см. forge_telegram::ws::protocol).
            muted: false,
            archived: false,
        },
        TgServerMsg::MessageEdited { message },
        TgServerMsg::MessageDeleted {
            dialog_id: "-1001234567890".into(),
            message_ids: vec![4242],
        },
        TgServerMsg::Typing {
            dialog_id: "777000".into(),
            user_id: "777000".into(),
            action: "typing".into(),
        },
        TgServerMsg::Online {
            user_id: "777000".into(),
            online: true,
            was_online: None,
        },
    ];
    for ev in &events {
        h.state.broadcast_event(ev.clone());
    }

    let mut got = Vec::new();
    for _ in 0..events.len() {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("событие не доехало")
            .unwrap()
            .unwrap();
        got.push(serde_json::from_str::<Value>(&frame.into_text().unwrap()).unwrap());
    }

    assert_eq!(got[0]["type"], "new_message");
    assert_eq!(got[0]["message"]["id"], 4242, "message_id — число");
    assert_eq!(
        got[0]["message"]["dialog_id"], "-1001234567890",
        "dialog_id — строка"
    );
    assert_eq!(got[0]["message"]["text"], "мяу");
    // Инвариант dto.rs: ключ есть всегда, даже когда значения нет.
    // `reply_to` (превью цитаты) в списке обязателен: фронт отличает
    // `null` («ничему не отвечает») от `undefined` («поле не приехало —
    // старый сервер»), и без ключа цитаты молча пропали бы.
    for key in ["sender", "media", "reply_to_id", "reply_to", "edit_date"] {
        assert!(
            got[0]["message"].get(key).is_some(),
            "ключ {key} обязан быть в кадре: {}",
            got[0]
        );
    }
    assert_eq!(got[1]["type"], "message_edited");
    assert_eq!(got[2]["type"], "message_deleted");
    assert_eq!(got[2]["message_ids"][0], 4242);
    assert_eq!(got[3]["type"], "typing");
    assert_eq!(got[3]["user_id"], "777000");
    assert_eq!(got[4]["type"], "online");
    assert_eq!(got[4]["online"], true);

    forge_telegram::shutdown(&h.state).await;
}

/// Фоновые воркеры поднимаются и штатно гаснут.
///
/// Без кредов и сессии восстанавливать нечего, но updates-supervisor
/// запускается ВСЕГДА (он сам ждёт логина). Проверяется, что воркер ровно
/// один, он не мешает работать роутам, и `shutdown` завершается быстро — то
/// есть supervisor реагирует на CancellationToken, а не висит до abort'а.
#[tokio::test]
async fn test_workers_start_and_stop_cleanly() {
    let h = start_harness().await;
    forge_telegram::spawn_workers(&h.state).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.state.workers.lock().await.len(),
        3,
        "без сессии запускаются updates-supervisor, подчистка медиа-кэша и GC загрузок"
    );

    // Плагин продолжает обслуживать запросы, пока воркер живёт.
    let resp = reqwest::get(format!("http://{}/api/telegram/status", h.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let started = std::time::Instant::now();
    forge_telegram::shutdown(&h.state).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown затянулся на {:?} — supervisor не слушает токен",
        started.elapsed()
    );
    assert!(h.state.workers.lock().await.is_empty());

    // Идемпотентность: повторный shutdown не паникует.
    forge_telegram::shutdown(&h.state).await;
}

/// Второе достижимое без сети состояние: креды есть, сессии нет.
///
/// Разница с `not_configured` принципиальна для фронта: `disconnected`
/// показывает форму «введите телефон», `not_configured` — форму
/// «введите api_id/api_hash». И в обоих случаях `api_hash` НЕ должен
/// появляться в ответе.
#[tokio::test]
async fn test_status_is_disconnected_with_credentials() {
    let h = start_harness_configured().await;
    let resp = reqwest::get(format!("http://{}/api/telegram/status", h.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["enabled"], true);
    assert_eq!(body["state"], "disconnected");
    assert!(body["user"].is_null());
    assert!(
        !raw.contains("0123456789abcdef"),
        "api_hash не должен уезжать на фронт: {raw}"
    );

    // Data-роуты в этом состоянии отвечают так же, как в not_configured:
    // «нет авторизации», а не «нет кредов».
    let resp = reqwest::get(format!("http://{}/api/telegram/dialogs", h.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");

    forge_telegram::shutdown(&h.state).await;
}

/// `POST /api/telegram/upload` без авторизации: 409 и, главное, БЕЗ чтения
/// тела.
///
/// Запрос отправляется вручную по TCP: заголовок `Content-Length` заявляет
/// 64 МиБ, а реально уходит десяток байт. Если бы хендлер буферизовал тело
/// (или ждал его целиком), ответа не было бы вовсе — тест упал бы по
/// таймауту. Проверяется именно тот порядок, ради которого он написан в
/// routes/upload.rs: авторизация → Content-Length → лимит → всё остальное.
#[tokio::test]
async fn test_upload_rejects_before_reading_the_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness().await;
    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let declared = 64 * 1024 * 1024;
    let head = format!(
        "POST /api/telegram/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-File-Name: photo.jpg\r\n\
         Content-Length: {declared}\r\n\
         Connection: close\r\n\r\n",
        h.addr
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    // Десять байт вместо заявленных 64 МиБ — тело намеренно не дописывается.
    sock.write_all(b"0123456789").await.unwrap();
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut raw)).await;
    assert!(
        read.is_ok(),
        "ответ не пришёл за 5с — значит сервер ждал тело целиком"
    );
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 409"), "ожидался 409, получено: {text}");
    assert!(
        text.contains("\"not_authorized\""),
        "ожидался код not_authorized: {text}"
    );

    forge_telegram::shutdown(&h.state).await;
}

/// `POST /api/telegram/upload` с телом обычным reqwest'ом — тот же 409.
///
/// Дублирует предыдущий тест на «нормальном» клиенте: сырой TCP проверяет
/// момент отказа, а этот — что обычный путь фронта тоже получает контрактное
/// тело ошибки, а не голый статус.
#[tokio::test]
async fn test_upload_without_auth_returns_contract_body() {
    let h = start_harness().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/telegram/upload", h.addr))
        .header("Content-Type", "application/octet-stream")
        .header("X-File-Name", "photo.jpg")
        .body(vec![1u8, 2, 3, 4])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");
    assert!(!body["error"]["message"].as_str().unwrap_or("").is_empty());

    forge_telegram::shutdown(&h.state).await;
}

/// Молчащий клиент отключается по idle-таймауту.
///
/// Тайминги укорочены до сотен миллисекунд (`TelegramState::with_ws_timings`),
/// поэтому проверяется НАСТОЯЩИЙ цикл `handle_socket`, а не его пересказ.
/// Клиент получает ping'и, но не отвечает `pong` — сервер обязан закрыть
/// соединение.
#[tokio::test]
async fn test_ws_closes_idle_connection_without_pong() {
    let h = start_harness_with(false, Some((Duration::from_millis(120), Duration::from_millis(400))))
        .await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let mut saw_ping = false;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = ws.next().await {
            match frame {
                Ok(Message::Text(t)) => {
                    if serde_json::from_str::<Value>(&t).ok().and_then(|v| {
                        v.get("type").and_then(|t| t.as_str()).map(str::to_string)
                    }) == Some("ping".into())
                    {
                        saw_ping = true;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => {}
            }
        }
        true
    })
    .await;

    assert!(closed.is_ok(), "соединение не закрылось по idle за 5с");
    assert!(saw_ping, "сервер обязан был прислать хотя бы один ping");
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "закрылись раньше idle-таймаута: {:?}",
        started.elapsed()
    );

    forge_telegram::shutdown(&h.state).await;
}

/// Клиент, который отвечает `pong`, живёт дольше idle-таймаута.
///
/// Обратная сторона предыдущего теста: без неё «закрываем всех подряд» тоже
/// прошло бы проверку.
#[tokio::test]
async fn test_ws_keeps_alive_while_client_ponging() {
    let idle = Duration::from_millis(400);
    let h = start_harness_with(false, Some((Duration::from_millis(120), idle))).await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();

    // Пропускаем снапшот login_state.
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap();

    // Держим соединение в три раза дольше idle-таймаута, отвечая pong'ом.
    let until = std::time::Instant::now() + idle * 3;
    while std::time::Instant::now() < until {
        ws.send(Message::Text(r#"{"type":"pong"}"#.to_string()))
            .await
            .expect("соединение закрылось, хотя клиент отвечает");
        tokio::time::sleep(idle / 4).await;
    }

    // Соединение живо: событие из broadcast всё ещё доезжает.
    h.state.broadcast_event(TgServerMsg::Resync);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_resync = false;
    while std::time::Instant::now() < deadline && !saw_resync {
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("нет кадров — соединение мертво")
            .expect("поток закрылся")
            .expect("ошибка чтения");
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "resync" {
                saw_resync = true;
            }
        }
    }
    assert!(saw_resync, "живое соединение обязано доставлять события");

    forge_telegram::shutdown(&h.state).await;
}

/// Без флага `--telegram` роутов плагина не существует.
///
/// Это ГЛАВНЫЙ opt-in-инвариант фичи: `register_routes` не вызывается, и
/// сервер обязан отвечать 404 на весь префикс — включая WS. Тест собирает
/// роутер ровно так же, как `main.rs` без флага.
#[tokio::test]
async fn test_routes_absent_without_the_plugin() {
    let app: Router = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Сервер жив.
    let resp = reqwest::get(format!("http://{addr}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);

    for path in [
        "/api/telegram/status",
        "/api/telegram/dialogs",
        "/api/telegram/dialogs/123/messages",
        "/api/telegram/dialogs/123/messages/7",
        "/api/telegram/dialogs/123/messages/7/callback",
        "/api/telegram/upload",
        "/api/telegram/media/123/7",
        "/api/telegram/logout",
    ] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(resp.status(), 404, "{path} обязан быть 404 без плагина");
    }

    // WS-апгрейд тоже не проходит.
    let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/telegram")).await;
    assert!(ws.is_err(), "/ws/telegram не должен существовать без плагина");
}

/// Роут нажатия inline-кнопки бота реально примонтирован, и монтирован
/// именно как `POST`.
///
/// Проверка «не 404» здесь не придирка: путь двухпараметрический
/// (`:id` + `:mid`) и лежит ПОД уже существующим
/// `/api/telegram/dialogs/:id/messages`. Опечатка в шаблоне или конфликт
/// имён параметров в axum дали бы либо 404 на живом сервере, либо панику
/// при сборке роутера — и то, и другое проявилось бы только в рантайме у
/// пользователя.
///
/// 405 на `GET` — вторая половина того же: он доказывает, что путь
/// СУЩЕСТВУЕТ (иначе был бы 404) и что чтением его дёрнуть нельзя.
#[tokio::test]
async fn test_callback_route_is_mounted_as_post_only() {
    let h = start_harness().await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();
    let path = "/api/telegram/dialogs/123/messages/7/callback";

    // GET по существующему пути ⇒ 405, а не 404.
    let resp = client.get(format!("{base}{path}")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 405, "GET обязан быть 405, а не 404");

    // PUT/DELETE — тоже 405: роут смонтирован ровно на один метод.
    for method in [reqwest::Method::PUT, reqwest::Method::DELETE] {
        let resp = client
            .request(method.clone(), format!("{base}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 405, "{method}");
    }

    // POST доходит до хендлера: 409 из общей лестницы проверок — значит
    // роут не просто существует, а ведёт в наш код.
    let resp = client
        .post(format!("{base}{path}"))
        .json(&json!({"data": "YQ=="}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");

    // И он не перехватывает соседний `POST /messages` — тот отвечает своим
    // же кодом, а не «callback без data».
    let resp = client
        .post(format!("{base}/api/telegram/dialogs/123/messages"))
        .json(&json!({"text": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);

    forge_telegram::shutdown(&h.state).await;
}

/// Роут «одно сообщение по id» примонтирован и не задел соседей.
///
/// Три пути делят префикс `/api/telegram/dialogs/:id/messages`: сама
/// история (GET/POST), одиночное сообщение (GET `…/:mid`) и нажатие кнопки
/// бота (POST `…/:mid/callback`). Ошибка в шаблоне или конфликт имён
/// параметров в axum дали бы либо панику при сборке роутера, либо 404/405
/// на живом сервере — и только у пользователя.
///
/// Поэтому проверяется вся тройка сразу: новый GET доходит до хендлера,
/// POST на него — 405 (а не «упал в соседний роут»), и оба соседа
/// продолжают отвечать своими кодами.
#[tokio::test]
async fn test_single_message_route_is_mounted_without_breaking_neighbours() {
    let h = start_harness().await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();
    let path = "/api/telegram/dialogs/123/messages/7";

    // GET доходит до хендлера: 409 из общей лестницы проверок — значит роут
    // не просто существует, а ведёт в наш код.
    let resp = client.get(format!("{base}{path}")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 409, "GET обязан вести в хендлер");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");

    // Записи по этому пути нет: роут смонтирован ровно на один метод.
    for method in [
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::DELETE,
    ] {
        let resp = client
            .request(method.clone(), format!("{base}{path}"))
            .json(&json!({"text": "hi"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            405,
            "{method} {path}: не 405 — значит путь пересёкся с соседним роутом"
        );
    }

    // Сосед сверху: отправка сообщения в диалог по-прежнему работает.
    let resp = client
        .post(format!("{base}/api/telegram/dialogs/123/messages"))
        .json(&json!({"text": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409, "POST /messages сломан");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");

    // Сосед снизу: нажатие кнопки бота на более длинном пути не перехвачено
    // новым роутом (иначе POST отвечал бы 405 от GET-only ветки).
    let resp = client
        .post(format!("{base}{path}/callback"))
        .json(&json!({"data": "YQ=="}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409, "callback-роут сломан");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_authorized");

    forge_telegram::shutdown(&h.state).await;
}

/// Авторизованное (но не подключённое) состояние: чтение сообщения по id
/// проходит авторизацию и упирается в отсутствие MTProto — `503`, а не 404
/// по мусорному `:mid`.
///
/// Это половина контракта фронта: по `not_connected` он показывает
/// RECONNECTING и повторяет попытку позже, а по `unknown_message` — пишет
/// «сообщение недоступно» и больше не пробует. Перепутать их значит либо
/// вечно долбить сервер, либо соврать пользователю про удалённое сообщение.
#[tokio::test]
async fn test_single_message_without_connection_is_not_connected() {
    let h = start_harness().await;
    authorize(&h.state).await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();

    for path in [
        "/api/telegram/dialogs/777000/messages/7",
        // Мусорный и вырожденный `:mid`: до их разбора дело не доходит.
        "/api/telegram/dialogs/777000/messages/not-a-number",
        "/api/telegram/dialogs/777000/messages/0",
    ] {
        let resp = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 503, "{path}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], "not_connected", "{path}");
        assert!(got["error"]["retry_after_secs"].is_null(), "{path}");
    }

    forge_telegram::shutdown(&h.state).await;
}

/// Авторизованное (но не подключённое) состояние: нажатие кнопки проходит
/// авторизацию и упирается в отсутствие MTProto — `503 not_connected`, а
/// не 404 и не 500.
///
/// Здесь же проверяется порядок «сначала соединение, потом разбор тела»:
/// заведомо мусорный `:mid` и пустое тело обязаны дать тот же 503, иначе
/// клиент по коду ошибки не отличит «переподключись» от «данные кривые».
#[tokio::test]
async fn test_callback_without_connection_is_not_connected() {
    let h = start_harness().await;
    authorize(&h.state).await;
    let base = format!("http://{}", h.addr);
    let client = reqwest::Client::new();

    for (path, body) in [
        (
            "/api/telegram/dialogs/777000/messages/7/callback",
            json!({"data": "YQ=="}),
        ),
        (
            "/api/telegram/dialogs/777000/messages/7/callback",
            json!({"game": true}),
        ),
        // Мусорный `:mid` и пустое тело — до их разбора дело не доходит.
        (
            "/api/telegram/dialogs/777000/messages/not-a-number/callback",
            json!({}),
        ),
    ] {
        let resp = client
            .post(format!("{base}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 503, "{path} {body}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], "not_connected", "{path} {body}");
        assert!(got["error"]["retry_after_secs"].is_null());
    }

    forge_telegram::shutdown(&h.state).await;
}

/// Переводит состояние в `authorized` без сети — ровно так, как это делает
/// восстановление сессии. Runtime (MTProto-клиент) при этом НЕ создаётся:
/// это легальное состояние «сессия есть, соединения нет».
async fn authorize(state: &Arc<TelegramState>) {
    state
        .set_login(forge_telegram::login::LoginFlow::Authorized {
            user: forge_telegram::login::TgUserDto {
                id: "42".into(),
                username: Some("neo".into()),
                first_name: Some("Нео".into()),
                phone: None,
            },
        })
        .await;
}

/// Отставший подписчик получает `resync`, а соединение живёт дальше.
///
/// Runtime `#[tokio::test]` однопоточный, поэтому синхронный цикл
/// `broadcast_event` без единого await гарантированно переполняет канал
/// (ёмкость [`forge_telegram::state::BROADCAST_CAPACITY`]) раньше, чем
/// handler успеет прочитать хоть одно событие. Это точная модель реального
/// шторма апдейтов при медленной вкладке.
#[tokio::test]
async fn test_ws_lagged_client_gets_resync_and_survives() {
    let h = start_harness().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    // Снапшот login_state: handler дошёл до цикла select.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();

    // Переполняем канал БЕЗ await: приёмник не успевает ни разу проснуться.
    for _ in 0..(forge_telegram::state::BROADCAST_CAPACITY + 500) {
        h.state.broadcast_event(TgServerMsg::Ping);
    }
    // Маркер после переполнения: он обязан доехать ПОСЛЕ resync.
    h.state.broadcast_event(TgServerMsg::Typing {
        dialog_id: "1".into(),
        user_id: "1".into(),
        action: "marker".into(),
    });

    let mut saw_resync = false;
    let mut resync_before_marker = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("поток кадров иссяк до маркера")
            .expect("соединение закрылось — Lagged не должен рвать WS")
            .expect("ошибка чтения кадра");
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            match v["type"].as_str() {
                Some("resync") => saw_resync = true,
                Some("typing") if v["action"] == "marker" => {
                    resync_before_marker = saw_resync;
                    break;
                }
                _ => {}
            }
        }
    }
    assert!(saw_resync, "отставший клиент обязан получить resync");
    assert!(
        resync_before_marker,
        "resync обязан прийти раньше событий, отправленных после переполнения"
    );

    forge_telegram::shutdown(&h.state).await;
}

/// Мусорные, бинарные, гигантские и протокольные ping-кадры от клиента не
/// рвут соединение И сбрасывают idle-таймер.
///
/// Реальный сценарий: страница с более новой версией фронта шлёт кадры,
/// которых сервер ещё не знает. Соединение обязано пережить их и не
/// закрыться по idle (любой кадр — признак жизни клиента).
#[tokio::test]
async fn test_ws_survives_garbage_binary_and_huge_frames() {
    let idle = Duration::from_millis(500);
    let h = start_harness_with(false, Some((Duration::from_millis(120), idle))).await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    // Снапшот login_state.
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap();

    // Три idle-периода шлём только «неправильные» кадры.
    let until = std::time::Instant::now() + idle * 3;
    let mut i = 0usize;
    while std::time::Instant::now() < until {
        let frame = match i % 4 {
            0 => Message::Text("{это не json".to_string()),
            // Невалидный UTF-8: бинарный кадр не должен трактоваться как текст.
            1 => Message::Binary(vec![0xff, 0xfe, 0x00, 0x9f]),
            // Полмегабайта мусора — заведомо больше любого штатного кадра.
            2 => Message::Text("x".repeat(512 * 1024)),
            _ => Message::Ping(b"hb".to_vec()),
        };
        ws.send(frame)
            .await
            .expect("сервер оборвал соединение на мусорном кадре");
        i += 1;
        // Подчищаем входящие server-ping'и, чтобы буферы не росли.
        while let Ok(Some(Ok(_))) =
            tokio::time::timeout(Duration::from_millis(10), ws.next()).await
        {}
        tokio::time::sleep(Duration::from_millis(90)).await;
    }

    // Соединение живо: маркер из broadcast доезжает.
    h.state.broadcast_event(TgServerMsg::Typing {
        dialog_id: "1".into(),
        user_id: "1".into(),
        action: "marker".into(),
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw_marker = false;
    while std::time::Instant::now() < deadline && !saw_marker {
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("нет кадров — соединение мертво")
            .expect("поток закрылся после мусорных кадров")
            .expect("ошибка чтения");
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "typing" && v["action"] == "marker" {
                saw_marker = true;
            }
        }
    }
    assert!(
        saw_marker,
        "после мусорных кадров соединение обязано остаться рабочим"
    );

    forge_telegram::shutdown(&h.state).await;
}

/// Graceful shutdown сервера закрывает WS сразу, а не оставляет вкладку
/// висеть до истечения таймаутов.
#[tokio::test]
async fn test_ws_shutdown_closes_client_promptly() {
    let h = start_harness().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();

    let started = std::time::Instant::now();
    forge_telegram::shutdown(&h.state).await;

    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(_))) | None => return true,
                Some(Err(_)) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "клиент не увидел закрытия за 2с после shutdown"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "закрытие затянулось: {:?}",
        started.elapsed()
    );
}

/// Апгрейд не ломается от query-параметров: и с токеном + неизвестными
/// полями, и с пустым `token=`.
///
/// Реальный случай — реконнект-URL фронта, куда позже допишут параметры;
/// старый сервер обязан их игнорировать, а не отвечать 400.
#[tokio::test]
async fn test_ws_upgrade_tolerates_token_and_unknown_query() {
    let h = start_harness().await;
    for query in ["?token=abc&v=2", "?token="] {
        let (mut ws, _resp) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram{query}", h.addr))
                .await
                .unwrap_or_else(|e| panic!("апгрейд с query `{query}` упал: {e}"));
        let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("первый кадр не пришёл")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&first.into_text().unwrap()).unwrap();
        assert_eq!(v["type"], "login_state", "query `{query}`");
    }
    forge_telegram::shutdown(&h.state).await;
}

/// Один broadcast — все вкладки. Две параллельные WS-подписки получают одно
/// и то же событие (на этом держится unread-бейдж в нескольких окнах).
#[tokio::test]
async fn test_ws_broadcast_reaches_multiple_clients() {
    let h = start_harness().await;
    let (mut ws1, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    let (mut ws2, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/telegram", h.addr))
        .await
        .unwrap();
    // Оба снапшота.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws1.next())
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), ws2.next())
        .await
        .unwrap();

    h.state.broadcast_event(TgServerMsg::Read {
        dialog_id: "7".into(),
        max_id: 1,
        direction: "out".into(),
    });
    for ws in [&mut ws1, &mut ws2] {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("событие не доехало до одного из подписчиков")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        assert_eq!(v["type"], "read");
        assert_eq!(v["dialog_id"], "7");
    }

    forge_telegram::shutdown(&h.state).await;
}

/// Третье достижимое без сети состояние: `authorized`, но MTProto-клиента
/// нет (сессия восстановлена, сеть упала).
///
/// Контракт: status отдаёт пользователя (id — СТРОКОЙ), data-роуты отвечают
/// `503 not_connected` («повтори позже»), а не `409 not_authorized`
/// (который выкинул бы пользователя на форму логина).
#[tokio::test]
async fn test_status_authorized_without_connection() {
    let h = start_harness().await;
    authorize(&h.state).await;

    let resp = reqwest::get(format!("http://{}/api/telegram/status", h.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["state"], "authorized");
    assert_eq!(body["user"]["id"], "42", "id пользователя — строка");
    assert_eq!(body["user"]["username"], "neo");
    assert!(body["error"].is_null());

    for path in [
        "/api/telegram/dialogs",
        "/api/telegram/dialogs/123/messages",
    ] {
        let resp = reqwest::get(format!("http://{}{path}", h.addr)).await.unwrap();
        assert_eq!(resp.status(), 503, "{path}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], "not_connected", "{path}");
    }

    forge_telegram::shutdown(&h.state).await;
}

/// Стриминговая загрузка без `Content-Length` (Transfer-Encoding: chunked)
/// отклоняется по проводу кодом `411 length_required`.
///
/// Это реальный запрос `fetch()` с ReadableStream-телом. До Фазы 5 такой
/// случай был покрыт только unit-тестом; здесь проверяется, что и hyper
/// доносит запрос без Content-Length, и ответ приходит ДО чтения тела.
#[tokio::test]
async fn test_upload_chunked_without_content_length_is_411() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness().await;
    authorize(&h.state).await;

    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let head = format!(
        "POST /api/telegram/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: a.bin\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\n\
         5\r\nhello\r\n0\r\n\r\n",
        h.addr
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut raw)).await;
    assert!(read.is_ok(), "ответ на chunked-запрос не пришёл за 5с");
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.starts_with("HTTP/1.1 411"),
        "ожидался 411, получено: {text}"
    );
    assert!(
        text.contains("\"length_required\""),
        "ожидался код length_required: {text}"
    );

    forge_telegram::shutdown(&h.state).await;
}

/// `Content-Length` больше лимита ⇒ `413 too_large` ДО чтения тела.
///
/// Тело вообще не отправляется: если бы сервер его ждал, тест упал бы по
/// таймауту. Именно так фронт узнаёт об отказе мгновенно, а не после
/// заливки гигабайтов.
#[tokio::test]
async fn test_upload_over_limit_is_413_before_reading_the_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness().await;
    authorize(&h.state).await;
    let declared = h.state.config.max_upload_bytes + 1;

    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let head = format!(
        "POST /api/telegram/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: big.bin\r\n\
         Content-Length: {declared}\r\n\
         Connection: close\r\n\r\n",
        h.addr
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut raw)).await;
    assert!(read.is_ok(), "413 обязан прийти без единого байта тела");
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.starts_with("HTTP/1.1 413"),
        "ожидался 413, получено: {text}"
    );
    assert!(text.contains("\"too_large\""), "ожидался код too_large: {text}");

    forge_telegram::shutdown(&h.state).await;
}

/// Полный happy-path загрузки по проводу: percent-encoded кириллица+эмодзи
/// в имени, staging на диске в tempdir, запись в реестре.
///
/// Заодно фиксирует контракт «staging не требует MTProto-соединения»:
/// состояние authorized, runtime отсутствует — и загрузка всё равно
/// успешна (обрыв связи не заставляет заново выбирать файл).
#[tokio::test]
async fn test_upload_stages_unicode_name_end_to_end() {
    let h = start_harness().await;
    authorize(&h.state).await;

    // encodeURIComponent("фото 🌍pic.jpg")
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/telegram/upload", h.addr))
        .header("Content-Type", "application/octet-stream")
        .header(
            "X-Forge-File-Name",
            "%D1%84%D0%BE%D1%82%D0%BE%20%F0%9F%8C%8Dpic.jpg",
        )
        .body(&b"abc123"[..])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["file_name"], "фото 🌍pic.jpg");
    assert_eq!(body["size"], 6);
    assert_eq!(body["mime"], "image/jpeg");

    let id = body["upload_id"].as_str().unwrap();
    let entry = h.state.upload_entry(id).await.expect("запись в реестре");
    assert_eq!(entry.file_name, "фото 🌍pic.jpg");
    assert_eq!(tokio::fs::read(&entry.path).await.unwrap(), b"abc123");
    assert!(
        entry.path.starts_with(&h.state.config.cache_dir),
        "staging обязан лежать в cache_dir стенда: {}",
        entry.path.display()
    );

    forge_telegram::shutdown(&h.state).await;
}

/// `TelegramConfig::load()` с временными путями: не паникует, не создаёт
/// ничего в реальном `~/.config/forge` и не подтягивает креды с машины.
///
/// Изоляция полная: `FORGE_TELEGRAM_CONFIG_PATH` указывает в tempdir, иначе
/// `load()` читал бы реальный `~/.config/forge/telegram.toml` разработчика и
/// проверка `!has_credentials()` зависела бы от машины.
///
/// Env-переменные процессные, поэтому тест держит их только на время
/// вызова и восстанавливает прежние значения.
#[tokio::test]
async fn test_config_load_with_temp_paths() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("nested/telegram.toml");
    let session = dir.path().join("nested/telegram.session");
    let cache = dir.path().join("nested/cache");

    let saved_config = std::env::var("FORGE_TELEGRAM_CONFIG_PATH").ok();
    let saved_session = std::env::var("FORGE_TELEGRAM_SESSION_PATH").ok();
    let saved_cache = std::env::var("FORGE_TELEGRAM_CACHE_DIR").ok();
    std::env::set_var("FORGE_TELEGRAM_CONFIG_PATH", &config);
    std::env::set_var("FORGE_TELEGRAM_SESSION_PATH", &session);
    std::env::set_var("FORGE_TELEGRAM_CACHE_DIR", &cache);

    let cfg = TelegramConfig::load();
    assert_eq!(cfg.session_path, session);
    assert_eq!(cfg.cache_dir, cache);
    assert!(cfg.validate().is_ok());
    assert!(cache.is_dir(), "cache_dir создаётся при загрузке конфига");
    assert!(!cfg.has_credentials(), "кредов во временном окружении нет");

    match saved_config {
        Some(v) => std::env::set_var("FORGE_TELEGRAM_CONFIG_PATH", v),
        None => std::env::remove_var("FORGE_TELEGRAM_CONFIG_PATH"),
    }
    match saved_session {
        Some(v) => std::env::set_var("FORGE_TELEGRAM_SESSION_PATH", v),
        None => std::env::remove_var("FORGE_TELEGRAM_SESSION_PATH"),
    }
    match saved_cache {
        Some(v) => std::env::set_var("FORGE_TELEGRAM_CACHE_DIR", v),
        None => std::env::remove_var("FORGE_TELEGRAM_CACHE_DIR"),
    }
}
