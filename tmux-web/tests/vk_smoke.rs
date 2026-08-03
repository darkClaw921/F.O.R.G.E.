//! Смоук-тесты плагина VK ПО ПРОВОДУ (близнец `telegram_smoke.rs`).
//!
//! Отличие от юнит-тестов внутри `plugins/vk/src/**`: здесь поднимается
//! НАСТОЯЩИЙ axum-сервер на свободном порту и запросы идут через реальный
//! HTTP-клиент. Это ловит ровно тот класс дефектов, который unit-тесты с
//! `oneshot` пропускают: несмонтированный роут, конфликт шаблонов путей,
//! сломанный WS-апгрейд, порядок слоёв, отказ ДО чтения тела.
//!
//! # Что покрыто
//!
//! **Opt-in.** [`test_routes_absent_without_the_plugin`] — ГЛАВНЫЙ инвариант
//! фичи: собираем роутер БЕЗ `register_routes` и убеждаемся, что весь префикс
//! `/api/vk/*` и `/ws/vk` отвечают 404. Именно на этом 404 держится гейт
//! кнопки Ⓥ во фронте, поэтому проверка не косметическая: подмонтируй
//! кто-нибудь `/api/vk/status` мимо флага — вкладка появилась бы у всех, кто
//! запускает devforge без `--vk`.
//!
//! **Состояния логина.** `not_configured`, `disconnected`, `invalid_token`,
//! `no_messages_scope`, `captcha_required`, `authorized` — все достижимы без
//! сети (`VkState::set_login`) и все проверены на «токен не уезжает наружу».
//!
//! **Лестница проверок** (`авторизация 409 → соединение 503 → резолв peer 404
//! → разбор параметров 400`) прогоняется по проводу на каждом data-роуте, в
//! том числе враждебными идентификаторами: иной порядок позволил бы
//! неавторизованному клиенту прощупывать валидность id по различию кодов.
//!
//! **Ответы VK** проверяются на локальном стенде ([`start_stub`]) — крошечном
//! axum-сервере, который отвечает на `POST {base_url}/{method}` заранее
//! положенным JSON. Он нужен ровно там, где отказ рождается НА СТОРОНЕ VK и
//! иначе недостижим: код 15 → `no_messages_scope`, код 14 → конверт с
//! `captcha_sid` + проксирование картинки, код 9 → `retry_after_secs`,
//! `callback`-кнопка → `unsupported_button`. Полноценный стенд транспорта
//! живёт в `plugins/vk/tests/vk_stub.rs` и здесь не дублируется.
//!
//! **Загрузка.** 411 без `Content-Length`, 413 сверх лимита, 400 без
//! `X-Forge-File-Name`, 409 без авторизации — и все ДО чтения тела (сырой TCP
//! заявляет 64 МиБ и не досылает их: буферизующий хендлер повесил бы тест по
//! таймауту). Плюс happy-path staging с юникодным именем и «`upload_id`
//! тратится ровно один раз».
//!
//! **Медиа.** Форма параметров раньше авторизации (400), попадание в кэш БЕЗ
//! единого обращения к VK (и даже при погашенном здоровье), 404 `no_media`
//! на отсутствующем вложении.
//!
//! **WS.** Первый кадр `login_state`, heartbeat/idle-таймаут (тайминги
//! укорочены через `VkState::with_ws_timings`, иначе тест ждал бы боевую
//! минуту), `Lagged` → `resync` без разрыва, битый входящий кадр → игнор,
//! широковещание в несколько вкладок, закрытие по shutdown.
//!
//! **Конфиг.** Битый `vk.toml` не роняет `init` — плагин поднимается на
//! дефолтах.
//!
//! Тесты НЕ требуют сети и НЕ требуют токена. Пути конфига задаются прямо в
//! [`VkConfig`], поэтому домашний каталог пользователя не затрагивается — в
//! частности, живой `~/.config/forge/vk.toml` разработчика не читается и не
//! перезаписывается.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, State as AxState};
use axum::Router;
use forge_vk::config::{SecretToken, VkConfig};
use forge_vk::login::{LoginFlow, VkUserDto};
use forge_vk::media_cache::{self, CacheKey, MediaMeta};
use forge_vk::state::VkState;
use forge_vk::ws::protocol::VkServerMsg;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------------------------
// Стенд плагина.
// ---------------------------------------------------------------------------

/// Живой сервер с примонтированным плагином.
struct Harness {
    addr: SocketAddr,
    state: Arc<VkState>,
    _tempdir: tempfile::TempDir,
    _serve: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// `http://127.0.0.1:PORT` — основание для всех REST-запросов.
    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Настройки стенда. Собраны в структуру, а не в позиционные аргументы:
/// вариантов уже три, и `start(false, None, None)` на месте вызова не читался
/// бы вовсе.
#[derive(Default)]
struct Opts {
    /// Класть ли в конфиг access-токен. Единственный способ достичь состояния
    /// `disconnected` без сети: сетевой проверки токена стенд не делает.
    token: Option<String>,
    /// Куда ходить за VK. По умолчанию — заведомо закрытый порт.
    base_url: Option<String>,
    /// Укороченные `(heartbeat, idle)` для WS-тестов.
    ws: Option<(Duration, Duration)>,
}

/// Поднимает плагин на свободном порту. Конфиг полностью изолирован во
/// временном каталоге (никаких обращений к `~/.config/forge`).
async fn start_harness() -> Harness {
    start_harness_opts(Opts::default()).await
}

/// Стенд с токеном в конфиге.
async fn start_harness_with(token: Option<&str>) -> Harness {
    start_harness_opts(Opts {
        token: token.map(str::to_string),
        ..Opts::default()
    })
    .await
}

/// Стенд, авторизованный и смотрящий на локальный стенд VK.
///
/// Возвращает и [`Harness`], и [`Stub`]: тесты кладут в стенд ответы VK и
/// потом сверяют журнал вызовов — «сколько запросов стоила страница» это
/// такой же контракт, как коды ошибок, при бюджете 3 rps.
async fn start_authorized_with_stub() -> (Harness, Stub) {
    let stub = start_stub().await;
    let h = start_harness_opts(Opts {
        token: Some("vk1.a.TEST".into()),
        base_url: Some(stub.base_url()),
        ..Opts::default()
    })
    .await;
    authorize(&h.state).await;
    (h, stub)
}

/// Общий конструктор стенда.
async fn start_harness_opts(opts: Opts) -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let cfg = VkConfig {
        cache_dir: tempdir.path().join("cache"),
        config_path: tempdir.path().join("vk.toml"),
        // Заведомо недостижимый адрес по умолчанию: если какой-нибудь воркер
        // решит сходить в сеть на старте, он упрётся в connect-refused
        // локально, а не постучится в настоящий api.vk.com из CI.
        base_url: opts
            .base_url
            .unwrap_or_else(|| "http://127.0.0.1:1/method".to_string()),
        access_token: opts.token.map(SecretToken::new).unwrap_or_default(),
        ..VkConfig::default()
    };

    let state = match opts.ws {
        // `init` сам создаёт состояние боевым конструктором, поэтому для
        // укороченных таймингов состояние собирается явно. Валидация конфига
        // при этом не теряется — она вызывается тут же.
        Some((hb, idle)) => {
            cfg.validate().expect("конфиг стенда обязан быть валидным");
            let _ = cfg.ensure_dirs();
            VkState::with_ws_timings(cfg, hb, idle)
        }
        None => forge_vk::init(cfg).expect("init must succeed on a valid config"),
    };
    let app: Router = forge_vk::register_routes(Router::new(), state.clone());

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

/// Переводит состояние в `authorized` без сети — ровно так же, как это делает
/// успешная проверка токена. Сетевой вызов при этом НЕ делается: «аккаунт
/// привязан» и «связь с VK есть» у нас разные вещи (см. `ConnHealth`).
async fn authorize(state: &Arc<VkState>) {
    state
        .set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1234".into(),
                first_name: Some("Тест".into()),
                last_name: Some("Тестов".into()),
                screen_name: Some("id1234".into()),
            },
        })
        .await;
}

// ---------------------------------------------------------------------------
// Стенд «вместо api.vk.com».
// ---------------------------------------------------------------------------

/// Байты картинки captcha, которые стенд отдаёт по `/captcha.png`.
///
/// Достаточно сигнатуры PNG: тест проверяет, что байты доехали через ПРОКСИ
/// бэкенда, а не что браузер умеет их нарисовать.
const CAPTCHA_PNG: &[u8] = b"\x89PNG\r\n\x1a\n-forge-vk-captcha";

/// Состояние стенда: заготовленные ответы и журнал вызовов.
#[derive(Default)]
struct StubState {
    replies: Mutex<HashMap<String, Value>>,
    calls: Mutex<Vec<String>>,
}

/// Локальный сервер, изображающий `api.vk.com`.
///
/// Умышленно минимальный: конверты ответов и журнал вызовов. Транспортные
/// сценарии (троттлер, повторы, `failed:N` у LongPoll, трёхшаговая заливка,
/// стриминг медиа) живут во флагманском стенде крейта
/// `plugins/vk/tests/vk_stub.rs`; дублировать их здесь означало бы вторую
/// копию, которая начнёт расходиться с первой.
struct Stub {
    addr: SocketAddr,
    state: Arc<StubState>,
    _serve: tokio::task::JoinHandle<()>,
}

impl Stub {
    /// То, что кладётся в `VkConfig::base_url`.
    fn base_url(&self) -> String {
        format!("http://{}/method", self.addr)
    }

    /// Адрес картинки captcha «у VK» — стенд отдаёт её как настоящий сервер.
    fn captcha_url(&self) -> String {
        format!("http://{}/captcha.png", self.addr)
    }

    /// Ответ на конкретный метод API.
    fn reply(&self, method: &str, value: Value) {
        self.state
            .replies
            .lock()
            .unwrap()
            .insert(method.to_string(), value);
    }

    /// Конверт ошибки VK (напоминание: ошибка приезжает с HTTP 200).
    fn reply_error(&self, method: &str, code: i64, extra: Value) {
        let mut err = json!({"error_code": code, "error_msg": format!("stub error {code}")});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                err[k] = v.clone();
            }
        }
        self.reply(method, json!({ "error": err }));
    }

    /// Сколько раз позвали метод.
    fn count(&self, method: &str) -> usize {
        self.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }

    /// Весь журнал — для сообщений об ошибках.
    fn calls(&self) -> Vec<String> {
        self.state.calls.lock().unwrap().clone()
    }
}

/// Поднимает стенд на свободном порту.
async fn start_stub() -> Stub {
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .route("/method/:name", axum::routing::post(stub_method))
        .route("/captcha.png", axum::routing::get(stub_captcha))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    Stub {
        addr,
        state,
        _serve: serve,
    }
}

/// `POST /method/:name` — заготовленный ответ либо безобидный `{"response":1}`.
async fn stub_method(
    AxState(state): AxState<Arc<StubState>>,
    AxPath(name): AxPath<String>,
    _body: String,
) -> axum::Json<Value> {
    state.calls.lock().unwrap().push(name.clone());
    let reply = state
        .replies
        .lock()
        .unwrap()
        .get(&name)
        .cloned()
        .unwrap_or_else(|| json!({"response": 1}));
    axum::Json(reply)
}

/// `GET /captcha.png` — байты картинки «у VK».
async fn stub_captcha() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        CAPTCHA_PNG.to_vec(),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Opt-in.
// ---------------------------------------------------------------------------

/// ГЛАВНЫЙ ИНВАРИАНТ ФИЧИ: без `register_routes` плагина нет вовсе.
///
/// Проверяется по проводу, а не через `Router::oneshot`, намеренно: 404
/// должен приходить от того же стека, что обслуживает пользователя. Фронт
/// гейтит кнопку Ⓥ именно по 404 на `GET /api/vk/status` — если бы роут
/// существовал всегда и отвечал, скажем, `{"enabled":false}`, вкладка
/// появилась бы у каждого, кто запускает devforge без `--vk`.
#[tokio::test]
async fn test_routes_absent_without_the_plugin() {
    let app: Router = Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Сервер жив — иначе 404 ниже ничего не доказывали бы.
    let resp = reqwest::get(format!("http://{addr}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);

    // Весь контракт путей из шапки `plugins/vk/src/routes/mod.rs`.
    for path in [
        "/api/vk/status",
        "/api/vk/filters",
        "/api/vk/dialogs",
        "/api/vk/dialogs/123/messages",
        "/api/vk/dialogs/123/messages/7",
        "/api/vk/dialogs/123/messages/7/button",
        "/api/vk/dialogs/123/read",
        "/api/vk/dialogs/123/typing",
        "/api/vk/login/token",
        "/api/vk/login/verify",
        "/api/vk/logout",
        "/api/vk/captcha",
        "/api/vk/captcha/239",
        "/api/vk/media/123/7/0",
        "/api/vk/upload",
    ] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(resp.status(), 404, "{path} обязан быть 404 без плагина");
    }

    // И POST'ом тоже: 405 означал бы, что роут всё-таки существует.
    let client = reqwest::Client::new();
    for path in [
        "/api/vk/login/token",
        "/api/vk/logout",
        "/api/vk/upload",
        "/api/vk/dialogs/123/messages",
    ] {
        let resp = client
            .post(format!("http://{addr}{path}"))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "POST {path} обязан быть 404 без плагина");
    }

    // WS-апгрейд тоже не проходит.
    let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/vk")).await;
    assert!(ws.is_err(), "/ws/vk не должен существовать без плагина");
}

// ---------------------------------------------------------------------------
// Статус и состояния логина.
// ---------------------------------------------------------------------------

/// Плагин зарегистрирован, токена нет: 200 и `not_configured`.
#[tokio::test]
async fn test_status_is_not_configured() {
    let h = start_harness().await;
    let resp = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["enabled"], true);
    assert_eq!(body["login"]["state"], "not_configured");
    assert!(body["login"]["user"].is_null());
    assert!(body["login"]["error"].is_null());
    assert!(body["login"]["captcha_sid"].is_null());
    // Фронтовый планировщик (Ф4) настраивается по этому полю, а не по своей
    // копии константы.
    assert_eq!(body["rate_limit_rps"], 3);
    assert_eq!(body["api_version"], "5.199");
    // Подсказка «где взять токен» приходит с сервера: разметка живёт в кэше
    // PWA неделями, а правила выдачи scope `messages` у VK меняются.
    assert!(
        !body["hint"].as_str().unwrap_or_default().is_empty(),
        "форма логина без подсказки бесполезна: {body}"
    );

    forge_vk::shutdown(&h.state).await;
}

/// С токеном стартовое состояние — `disconnected`: `init` синхронный и сети
/// не трогает, проверка токена (Ф3) делается фоново.
#[tokio::test]
async fn test_status_is_disconnected_with_a_token() {
    let h = start_harness_with(Some("vk1.a.smoke")).await;
    let body: Value = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "disconnected");

    forge_vk::shutdown(&h.state).await;
}

/// Токен не уходит в ответ НИ В КАКОМ виде — ни целиком, ни хвостом.
///
/// Проверяется на сыром теле, а не на разобранном JSON: утечка могла бы
/// оказаться в любом поле, в том числе в новом, о котором тест не знает.
#[tokio::test]
async fn test_status_never_leaks_the_token_over_the_wire() {
    const TOKEN: &str = "vk1.a.SUPERSECRETVALUE0123456789";
    let h = start_harness_with(Some(TOKEN)).await;
    let raw = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(!raw.contains(TOKEN), "токен целиком в ответе: {raw}");
    assert!(!raw.contains("SUPERSECRET"), "часть токена в ответе: {raw}");
    assert!(!raw.contains("0123456789"), "хвост токена в ответе: {raw}");

    forge_vk::shutdown(&h.state).await;
}

/// Все состояния автомата логина доезжают до клиента ПО ПРОВОДУ — и ни одно
/// из них не тащит с собой токен.
///
/// Юнит-тест в `routes/mod.rs` проверяет то же на сериализации DTO; здесь
/// важно, что состояние переживает реальный HTTP-ответ и что не-авторизованные
/// состояния одинаково закрывают data-роуты кодом `not_authorized`, а не
/// каждым своим (иначе фронту пришлось бы знать их все).
#[tokio::test]
async fn test_login_states_travel_over_the_wire() {
    const TOKEN: &str = "vk1.a.STATE-CANARY-9876543210";
    let cases: Vec<(LoginFlow, &str)> = vec![
        (LoginFlow::Checking, "checking"),
        (
            LoginFlow::Disconnected {
                error: Some("connection reset".into()),
            },
            "disconnected",
        ),
        (
            LoginFlow::InvalidToken {
                error: "VK error 5".into(),
            },
            "invalid_token",
        ),
        (
            LoginFlow::NoMessagesScope {
                error: "VK error 15".into(),
            },
            "no_messages_scope",
        ),
        (
            LoginFlow::CaptchaRequired {
                sid: "239123".into(),
                img: "https://api.vk.com/captcha.php?sid=239123".into(),
            },
            "captcha_required",
        ),
    ];

    let h = start_harness_with(Some(TOKEN)).await;
    for (flow, kind) in cases {
        h.state.set_login(flow).await;
        let raw = reqwest::get(format!("{}/api/vk/status", h.base()))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(body["login"]["state"], kind, "{raw}");
        assert!(!raw.contains(TOKEN), "{kind}: токен в ответе: {raw}");
        assert!(!raw.contains("CANARY"), "{kind}: часть токена: {raw}");

        // Ни одно из этих состояний не является авторизованным.
        let resp = reqwest::get(format!("{}/api/vk/dialogs", h.base()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 409, "{kind}");
        let err: Value = resp.json().await.unwrap();
        assert_eq!(err["error"]["code"], "not_authorized", "{kind}");
    }

    // Captcha дополнительно несёт sid — фронт по нему строит адрес прокси.
    h.state
        .set_login(LoginFlow::CaptchaRequired {
            sid: "239123".into(),
            img: "https://api.vk.com/captcha.php?sid=239123".into(),
        })
        .await;
    let body: Value = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["captcha_sid"], "239123");

    forge_vk::shutdown(&h.state).await;
}

/// Авторизованный статус отдаёт пользователя, а `id` — СТРОКОЙ.
///
/// Инвариант «id — строки» держит весь фронт: `peer_id` беседы у VK выходит за
/// пределы точного целого в JS далеко не всегда, но однажды выйдет, и разные
/// написания одного id развели бы ключи кэша.
#[tokio::test]
async fn test_status_authorized_reports_the_user() {
    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;

    let body: Value = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "authorized");
    assert_eq!(body["login"]["user"]["id"], "1234");
    assert!(body["login"]["user"]["id"].is_string(), "id обязан быть строкой");
    assert_eq!(body["login"]["user"]["first_name"], "Тест");
    assert!(body["login"]["error"].is_null());

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Таблица «метод × путь × тело → статус + code».
// ---------------------------------------------------------------------------

/// Контракт ошибок в состоянии `not_configured` — по всем роутам разом.
///
/// Таблица, а не десяток тестов: важна именно ОДНОРОДНОСТЬ ответов. Каждая
/// строка — то, что фронт обязан уметь разобрать одной веткой.
#[tokio::test]
async fn test_routes_report_expected_error_codes() {
    let h = start_harness().await;
    let base = h.base();
    let client = reqwest::Client::new();

    // (метод, путь, тело, ожидаемый статус, ожидаемый код)
    let cases: &[(&str, &str, Value, u16, &str)] = &[
        // Логин: пустая строка — ошибка ввода, ДО того как что-то попадёт на
        // диск.
        (
            "POST",
            "/api/vk/login/token",
            json!({"access_token": ""}),
            400,
            "invalid_input",
        ),
        (
            "POST",
            "/api/vk/login/token",
            json!({"access_token": "   "}),
            400,
            "invalid_input",
        ),
        // Перепроверять нечего: токена нет.
        (
            "POST",
            "/api/vk/login/verify",
            json!({}),
            409,
            "not_authorized",
        ),
        // Ответ на captcha без пары sid/key — ошибка ввода.
        (
            "POST",
            "/api/vk/captcha",
            json!({"sid": "", "key": ""}),
            400,
            "invalid_input",
        ),
        (
            "POST",
            "/api/vk/captcha",
            json!({"sid": "239", "key": ""}),
            400,
            "invalid_input",
        ),
        // Data-роуты: аккаунт не привязан.
        ("GET", "/api/vk/filters", json!({}), 409, "not_authorized"),
        ("GET", "/api/vk/dialogs", json!({}), 409, "not_authorized"),
        (
            "GET",
            "/api/vk/dialogs?limit=20&offset=0&filter=unread",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/vk/dialogs/2000000001/messages",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/vk/dialogs/1234/messages",
            json!({"text": "привет"}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/vk/dialogs/1234/messages/7001",
            json!({}),
            409,
            "not_authorized",
        ),
        // Кнопка бота: авторизация раньше разбора `:mid` и тела.
        (
            "POST",
            "/api/vk/dialogs/1234/messages/7001/button",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/vk/dialogs/1234/messages/not-a-number/button",
            json!({"row": 0, "col": 0}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/vk/dialogs/1234/read",
            json!({"max_id": 7001}),
            409,
            "not_authorized",
        ),
        (
            "POST",
            "/api/vk/dialogs/1234/typing",
            json!({}),
            409,
            "not_authorized",
        ),
        // Загрузка: авторизация ДО заголовков размера и имени.
        (
            "POST",
            "/api/vk/upload",
            json!({}),
            409,
            "not_authorized",
        ),
        // Медиа: форма параметров — раньше авторизации (мусорный индекс это
        // ошибка клиента в любом состоянии логина, и по 400 ничего о чужих
        // беседах узнать нельзя).
        (
            "GET",
            "/api/vk/media/1234/7001/10",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "GET",
            "/api/vk/media/1234/7001/0?kind=thmub",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "GET",
            "/api/vk/media/абв/7001/0",
            json!({}),
            400,
            "invalid_input",
        ),
        (
            "GET",
            "/api/vk/media/1234/+7001/0",
            json!({}),
            400,
            "invalid_input",
        ),
        // А корректная форма — снова лестница: 409.
        (
            "GET",
            "/api/vk/media/1234/7001/0",
            json!({}),
            409,
            "not_authorized",
        ),
        (
            "GET",
            "/api/vk/media/1234/7001/0?kind=thumb",
            json!({}),
            409,
            "not_authorized",
        ),
        // Прокси картинки captcha: неизвестный sid — 404 с телом контракта, а
        // не пустой ответ роутера.
        ("GET", "/api/vk/captcha/239123", json!({}), 404, "no_media"),
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
            "{method} {path}: message обязателен — его показывают пользователю"
        );
        // Дополнительные поля конверта появляются ТОЛЬКО у своих кодов.
        assert!(
            got["error"]["retry_after_secs"].is_null(),
            "{method} {path}: retry_after_secs только у flood_wait"
        );
        assert!(
            got["error"]["captcha_sid"].is_null(),
            "{method} {path}: captcha_sid только у captcha_required"
        );
        assert!(
            got["error"]["redirect_uri"].is_null(),
            "{method} {path}: redirect_uri только у validation_required"
        );
    }

    // GET по POST-путям — 405, а НЕ 404: иначе фронт не отличит выключенный
    // плагин от опечатки в пути.
    for path in [
        "/api/vk/login/token",
        "/api/vk/login/verify",
        "/api/vk/logout",
        "/api/vk/captcha",
        "/api/vk/upload",
    ] {
        let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(resp.status(), 405, "GET {path}");
    }

    forge_vk::shutdown(&h.state).await;
}

/// ПОРЯДОК ПРОВЕРОК ИЗ КОНТРАКТА: авторизация — ПЕРВОЙ, до какого-либо
/// разбора `:id`, `:mid` и query.
///
/// Враждебные и вырожденные идентификаторы (path traversal, знак `+`,
/// переполнение i64, emoji, XSS-строка) обязаны получать тот же
/// `409 not_authorized`, что и честный id. Иной ответ позволил бы
/// неавторизованному клиенту прощупывать валидность идентификаторов по
/// различию кодов — то есть означал бы, что разбор ввода делается до
/// аутентификации.
#[tokio::test]
async fn test_hostile_dialog_ids_hit_auth_before_any_parsing() {
    let h = start_harness().await;
    let base = h.base();
    let client = reqwest::Client::new();

    let ids = [
        "..%2F..%2Fetc%2Fpasswd",
        "%2B79990000000",
        "+123",
        "99999999999999999999999999",
        "0",
        "%F0%9F%98%80",
        "%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        "null",
    ];
    for id in ids {
        for tail in [
            "messages",
            "messages?limit=abc&before_id=%F0%9F%98%80",
            "messages/7001",
            "messages/not-a-number",
        ] {
            let url = format!("{base}/api/vk/dialogs/{id}/{tail}");
            let resp = client.get(&url).send().await.unwrap();
            assert_eq!(
                resp.status().as_u16(),
                409,
                "{url}: авторизация обязана идти первой"
            );
            let got: Value = resp.json().await.unwrap();
            assert_eq!(got["error"]["code"], "not_authorized", "{url}");
        }

        // Мутирующие роуты — по тому же правилу, включая заведомо битое тело:
        // `Option<Json<T>>` не должен отвергать запрос раньше авторизации.
        for (tail, body) in [
            ("messages", r#"{"text":"привет"}"#),
            ("messages", "{ битый json"),
            ("read", "{}"),
            ("typing", r#"{"action":"typing"}"#),
        ] {
            let url = format!("{base}/api/vk/dialogs/{id}/{tail}");
            let resp = client
                .post(&url)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status().as_u16(),
                409,
                "POST {url}: авторизация обязана идти первой"
            );
            let got: Value = resp.json().await.unwrap();
            assert_eq!(got["error"]["code"], "not_authorized", "POST {url}");
        }
    }

    forge_vk::shutdown(&h.state).await;
}

/// Вторая и третья ступени лестницы ПО ПРОВОДУ: авторизация пройдена, связи
/// нет ⇒ `503 not_connected` (с причиной внутри), связь есть, а `:id` мусорный
/// ⇒ `404 unknown_dialog`.
///
/// Мусорный `:id` при погашенном здоровье всё равно получает 503, а не 404:
/// «переподключись» — честный совет, пока мы даже не знаем, есть ли такая
/// беседа.
#[tokio::test]
async fn test_ladder_503_then_404_after_authorization() {
    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;

    // Здоровье «упало прямо сейчас» ⇒ мы внутри грейса.
    h.state
        .note_down(forge_vk::dto::now_unix(), "connection reset")
        .await;
    for path in [
        "/api/vk/dialogs",
        "/api/vk/dialogs/1234/messages",
        "/api/vk/dialogs/..%2F..%2Fetc/messages",
        "/api/vk/dialogs/1234/messages/7001",
    ] {
        let resp = reqwest::get(format!("{}{path}", h.base())).await.unwrap();
        assert_eq!(resp.status(), 503, "{path}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], "not_connected", "{path}");
        assert!(
            got["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("connection reset"),
            "причина отказа обязана доехать до пользователя: {got}"
        );
    }

    // `/filters` — статичный: требует авторизации, но НЕ соединения (503 на
    // константный ответ был бы враньём) и не делает ни одного вызова VK.
    let resp = reqwest::get(format!("{}/api/vk/filters", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "фильтры не зависят от сети");
    let got: Value = resp.json().await.unwrap();
    let filters = got["filters"].as_array().unwrap();
    assert_eq!(
        filters
            .iter()
            .map(|f| f["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["all", "unread", "important", "unanswered"]
    );

    // Здоровье восстановлено ⇒ третья ступень: мусорный `:id` это 404.
    h.state.note_ok(forge_vk::dto::now_unix()).await;
    for id in ["..%2F..%2Fetc%2Fpasswd", "abc", "0", "%F0%9F%98%80"] {
        let resp = reqwest::get(format!("{}/api/vk/dialogs/{id}/messages", h.base()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "{id}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], "unknown_dialog", "{id}");
    }

    // Четвёртая ступень: всё пройдено — и только теперь разбираются параметры.
    for path in [
        "/api/vk/dialogs?limit=abc",
        "/api/vk/dialogs?offset=1e3",
        "/api/vk/dialogs/1234/messages?limit=2.5",
        "/api/vk/dialogs/1234/messages?before_id=99999999999999999999",
    ] {
        let resp = reqwest::get(format!("{}{path}", h.base())).await.unwrap();
        assert_eq!(resp.status(), 400, "{path}");
        let got: Value = resp.json().await.unwrap();
        assert_eq!(got["error"]["code"], "invalid_input", "{path}");
    }

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Логин: logout и прокси captcha.
// ---------------------------------------------------------------------------

/// `POST /logout` идемпотентен, стирает токен С ДИСКА и в VK не ходит.
///
/// Порядок «сначала диск, потом память» проверяется наблюдаемо: после первого
/// вызова файл конфига не содержит токена, а статус — `not_configured`.
/// Второй вызов (Ctrl-C по кнопке дважды, повтор фронта) обязан пройти так же.
#[tokio::test]
async fn test_logout_is_idempotent_and_wipes_the_token() {
    const TOKEN: &str = "vk1.a.LOGOUT-CANARY";
    let h = start_harness_with(Some(TOKEN)).await;
    authorize(&h.state).await;
    let cfg_path = h.state.config.config_path.clone();
    // Кладём токен на диск, как это делает вставка в форму.
    let mut cfg = h.state.effective_config().await;
    cfg.save_token(&SecretToken::new(TOKEN)).unwrap();
    assert!(
        std::fs::read_to_string(&cfg_path).unwrap().contains(TOKEN),
        "предусловие: токен обязан лежать в vk.toml"
    );

    let client = reqwest::Client::new();
    for attempt in 1..=2 {
        let resp = client
            .post(format!("{}/api/vk/logout", h.base()))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "попытка {attempt}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["state"], "not_configured", "попытка {attempt}");
    }

    let on_disk = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    assert!(!on_disk.contains(TOKEN), "токен остался в vk.toml: {on_disk}");

    let body: Value = reqwest::get(format!("{}/api/vk/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "not_configured");

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Ответы VK на стенде.
// ---------------------------------------------------------------------------

/// Пустая страница бесед и пустая история — 200 и контрактная форма.
///
/// Заодно счётчик вызовов: страница обязана стоить ОДИН метод API. При бюджете
/// 3 rps лишний вызов на странице — это треть секунды у пользователя.
#[tokio::test]
async fn test_empty_pages_are_a_200_contract() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply(
        "messages.getConversations",
        json!({"response": {"count": 0, "items": [], "profiles": [], "groups": []}}),
    );
    stub.reply(
        "messages.getHistory",
        json!({"response": {"count": 0, "items": [], "profiles": [], "groups": []}}),
    );
    stub.reply("users.get", json!({"response": []}));

    let body: Value = reqwest::get(format!("{}/api/vk/dialogs", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["dialogs"], json!([]));
    assert_eq!(body["has_more"], false);
    assert_eq!(
        stub.count("messages.getConversations"),
        1,
        "страница бесед обязана стоить один вызов: {:?}",
        stub.calls()
    );

    let body: Value = reqwest::get(format!("{}/api/vk/dialogs/1234/messages", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["messages"], json!([]));
    assert_eq!(body["has_more"], false);
    assert_eq!(
        stub.count("messages.getHistory"),
        1,
        "страница истории обязана стоить один вызов истории: {:?}",
        stub.calls()
    );

    forge_vk::shutdown(&h.state).await;
}

/// VK код 15 → `409 no_messages_scope`, а НЕ `not_authorized`.
///
/// Разница видна пользователю: «вставьте новый токен» бессмысленно, когда
/// токен исправен, но выдан без права `messages`. Это же имя — состояние
/// логина и код ошибки (одно из трёх замороженных решений плагина).
#[tokio::test]
async fn test_vk_error_15_becomes_no_messages_scope() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply_error("messages.getConversations", 15, json!({}));

    let resp = reqwest::get(format!("{}/api/vk/dialogs", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["error"]["code"], "no_messages_scope");
    assert!(
        got["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("messages"),
        "объяснение обязано называть недостающее право: {got}"
    );

    forge_vk::shutdown(&h.state).await;
}

/// VK код 14 → конверт `captcha_required` с `captcha_sid`, а картинка
/// забирается ЧЕРЕЗ НАШ ПРОКСИ.
///
/// Это проверка «браузер пользователя не ходит на серверы VK»: адрес картинки
/// сервер запоминает у себя, а наружу отдаёт только `sid`. Прокси после этого
/// обязан отдать байты и запретить кэширование — картинка одноразовая, и
/// закэшированная браузером она показала бы прошлую задачу к новому `sid`.
#[tokio::test]
async fn test_vk_error_14_yields_captcha_envelope_and_proxied_image() {
    let (h, stub) = start_authorized_with_stub().await;
    let img = stub.captcha_url();
    stub.reply_error(
        "messages.getConversations",
        14,
        json!({"captcha_sid": "239123", "captcha_img": img}),
    );

    let resp = reqwest::get(format!("{}/api/vk/dialogs", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["error"]["code"], "captcha_required");
    assert_eq!(got["error"]["captcha_sid"], "239123");
    assert!(
        got["error"]["retry_after_secs"].is_null(),
        "retry_after_secs только у flood_wait: {got}"
    );

    // Прокси знает про этот sid — потому что сервер запомнил адрес сам.
    let resp = reqwest::get(format!("{}/api/vk/captcha/239123", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "image/png"
    );
    assert!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("no-store"),
        "одноразовую картинку нельзя кэшировать"
    );
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), CAPTCHA_PNG);

    // А чужой sid прокси не открывает: иначе это открытый прокси (SSRF).
    let resp = reqwest::get(format!("{}/api/vk/captcha/999999", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "no_media"
    );

    forge_vk::shutdown(&h.state).await;
}

/// VK код 9 → `429 flood_wait` с `retry_after_secs` И заголовком `Retry-After`.
///
/// Ровно тот единственный код, у которого поле конверта заполнено — во всех
/// остальных строках таблицы выше оно проверено как `null`.
#[tokio::test]
async fn test_vk_flood_control_carries_retry_after() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply_error("messages.getConversations", 9, json!({}));

    let resp = reqwest::get(format!("{}/api/vk/dialogs", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    let retry_header = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["error"]["code"], "flood_wait");
    let secs = got["error"]["retry_after_secs"].as_u64().unwrap_or_default();
    assert!(secs > 0, "flood_wait без срока ожидания бесполезен: {got}");
    assert_eq!(
        retry_header.as_deref(),
        Some(secs.to_string().as_str()),
        "заголовок Retry-After обязан совпадать с полем конверта"
    );

    forge_vk::shutdown(&h.state).await;
}

/// Кнопка сообщества, которую под пользовательским токеном нажать нельзя, —
/// это `409 unsupported_button`, и ни одного `messages.send` при этом не
/// уходит.
///
/// Ограничение ПЛАТФОРМЫ, а не наша недоделка: `callback` требует официального
/// клиента. Отдельный код нужен, чтобы UI объяснил это словами, а не показал
/// «ошибка сервера» и не выглядел поломкой бота.
#[tokio::test]
async fn test_unsupported_button_is_409_and_sends_nothing() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply(
        "messages.getById",
        json!({"response": {"count": 1, "items": [{
            "id": 7001,
            "peer_id": 1234,
            "date": 1_700_000_000,
            "from_id": -55,
            "text": "выберите действие",
            "conversation_message_id": 12,
            "keyboard": {"inline": true, "buttons": [[
                {"color": "primary", "action": {"type": "callback", "label": "Подтвердить", "payload": "{\"cmd\":1}"}},
                {"color": "secondary", "action": {"type": "text", "label": "Отмена", "payload": "{\"cmd\":0}"}}
            ]]}
        }], "profiles": [], "groups": []}}),
    );
    stub.reply("users.get", json!({"response": []}));

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/vk/dialogs/1234/messages/7001/button",
            h.base()
        ))
        .json(&json!({"row": 0, "col": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["error"]["code"], "unsupported_button");
    assert_eq!(
        stub.count("messages.send"),
        0,
        "неподдерживаемая кнопка не имеет права отправить сообщение: {:?}",
        stub.calls()
    );

    // Координаты вне клавиатуры — ошибка ввода, а не «неподдерживаемый тип».
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/vk/dialogs/1234/messages/7001/button",
            h.base()
        ))
        .json(&json!({"row": 5, "col": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_input"
    );

    forge_vk::shutdown(&h.state).await;
}

/// Сообщение, которого в этой беседе нет, — `404 unknown_message`.
///
/// Сверка `peer_id` обязательна: `messages.getById` адресуется ГЛОБАЛЬНЫМ id,
/// и без неё роут отдавал бы сообщение из ЧУЖОЙ беседы тому, кто подобрал
/// номер.
#[tokio::test]
async fn test_message_from_another_dialog_is_404() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply(
        "messages.getById",
        json!({"response": {"count": 1, "items": [{
            "id": 7001,
            "peer_id": 999,
            "date": 1_700_000_000,
            "from_id": 999,
            "text": "чужое"
        }], "profiles": [], "groups": []}}),
    );

    let resp = reqwest::get(format!("{}/api/vk/dialogs/1234/messages/7001", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "unknown_message"
    );

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Медиа.
// ---------------------------------------------------------------------------

/// Попадание в кэш обслуживается БЕЗ единого обращения к VK — и даже при
/// погашенном здоровье.
///
/// Порядок в роуте именно такой (форма → авторизация → кэш → соединение → VK)
/// ради этого случая: сетевой сбой не должен гасить уже принятые картинки.
#[tokio::test]
async fn test_media_cache_hit_needs_no_vk_calls() {
    let (h, stub) = start_authorized_with_stub().await;
    let key = CacheKey::parse("1234", "7001", "0", Some("full")).unwrap();
    let bytes = b"forge-vk-cached-bytes".to_vec();
    media_cache::put_atomic(
        &h.state.config,
        &key,
        &bytes,
        &MediaMeta::now(
            Some("image/jpeg".into()),
            bytes.len() as u64,
            Some("кот.jpg".into()),
        ),
    )
    .await
    .unwrap();

    // Здоровье погашено: сетевой путь заведомо отвечал бы 503.
    h.state
        .note_down(forge_vk::dto::now_unix(), "connection reset")
        .await;

    let resp = reqwest::get(format!("{}/api/vk/media/1234/7001/0?kind=full", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "закэшированное медиа обязано отдаваться без сети");
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg")
    );
    assert!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("immutable"),
        "ключ кэша адресует неизменяемые байты"
    );
    let disposition = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        disposition.contains("filename*=UTF-8''"),
        "кириллическое имя обязано ехать в RFC 5987-форме: {disposition}"
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), bytes.as_slice());

    assert!(
        stub.calls().is_empty(),
        "попадание в кэш не имеет права звать VK: {:?}",
        stub.calls()
    );

    forge_vk::shutdown(&h.state).await;
}

/// У сообщения нет вложения с таким индексом ⇒ `404 no_media`.
///
/// Отдельный код от `unknown_message`: сообщение есть, просто картинки в нём
/// нет — фронт по этому коду убирает карточку, а не показывает «беседа
/// исчезла».
#[tokio::test]
async fn test_media_without_attachment_is_404_no_media() {
    let (h, stub) = start_authorized_with_stub().await;
    stub.reply(
        "messages.getById",
        json!({"response": {"count": 1, "items": [{
            "id": 7001,
            "peer_id": 1234,
            "date": 1_700_000_000,
            "from_id": 1234,
            "text": "просто текст",
            "attachments": []
        }], "profiles": [], "groups": []}}),
    );

    let resp = reqwest::get(format!("{}/api/vk/media/1234/7001/0", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "no_media"
    );

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Загрузка файлов.
// ---------------------------------------------------------------------------

/// `POST /api/vk/upload` без авторизации: 409 и, главное, БЕЗ чтения тела.
///
/// Запрос отправляется вручную по TCP: заголовок `Content-Length` заявляет
/// 64 МиБ, а реально уходит десяток байт. Если бы хендлер буферизовал тело
/// (или ждал его целиком), ответа не было бы вовсе — тест упал бы по таймауту.
/// Проверяется именно тот порядок, ради которого он написан в
/// `routes/upload.rs`: авторизация → Content-Length → лимит → имя → тело.
#[tokio::test]
async fn test_upload_rejects_before_reading_the_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness().await;
    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let declared = 64 * 1024 * 1024;
    let head = format!(
        "POST /api/vk/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: photo.jpg\r\n\
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
    assert!(text.starts_with("HTTP/1.1 409"), "ожидался 409: {text}");
    assert!(text.contains("\"not_authorized\""), "{text}");

    forge_vk::shutdown(&h.state).await;
}

/// Стриминговая загрузка без `Content-Length` (`Transfer-Encoding: chunked`)
/// отклоняется по проводу кодом `411 length_required`.
///
/// Это реальный запрос `fetch()` с ReadableStream-телом: без размера ни
/// проверить лимит, ни показать прогресс нечем.
#[tokio::test]
async fn test_upload_chunked_without_content_length_is_411() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;

    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let head = format!(
        "POST /api/vk/upload HTTP/1.1\r\n\
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
    assert!(text.starts_with("HTTP/1.1 411"), "ожидался 411: {text}");
    assert!(text.contains("\"length_required\""), "{text}");

    forge_vk::shutdown(&h.state).await;
}

/// `Content-Length` больше лимита ⇒ `413 too_large` ДО чтения тела.
///
/// Тело вообще не отправляется: если бы сервер его ждал, тест упал бы по
/// таймауту. Именно так фронт узнаёт об отказе мгновенно, а не после заливки
/// гигабайтов.
#[tokio::test]
async fn test_upload_over_limit_is_413_before_reading_the_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;
    let declared = h.state.config.max_upload_bytes + 1;

    let mut sock = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    let head = format!(
        "POST /api/vk/upload HTTP/1.1\r\n\
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
    assert!(text.starts_with("HTTP/1.1 413"), "ожидался 413: {text}");
    assert!(text.contains("\"too_large\""), "{text}");

    forge_vk::shutdown(&h.state).await;
}

/// Загрузка без `X-Forge-File-Name` ⇒ `400 invalid_input`.
///
/// Имя файла — не украшение: по нему выбирается MIME, а по MIME — ветка
/// конвейера (фото или документ). Без него сервер не знает, какой
/// upload-сервер просить у VK.
#[tokio::test]
async fn test_upload_without_the_file_name_header_is_400() {
    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;

    let resp = reqwest::Client::new()
        .post(format!("{}/api/vk/upload", h.base()))
        .header("content-type", "application/octet-stream")
        .body(vec![1u8, 2, 3, 4])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["error"]["code"], "invalid_input");
    assert!(
        got["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("x-forge-file-name"),
        "сообщение обязано называть недостающий заголовок: {got}"
    );

    // Пустое имя после декодирования — тоже ошибка ввода.
    let resp = reqwest::Client::new()
        .post(format!("{}/api/vk/upload", h.base()))
        .header("content-type", "application/octet-stream")
        .header("x-forge-file-name", "%20%20")
        .body(vec![1u8, 2, 3, 4])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_input"
    );

    forge_vk::shutdown(&h.state).await;
}

/// Полный happy-path staging по проводу: percent-encoded кириллица + эмодзи в
/// имени, файл в tempdir стенда, запись в реестре.
///
/// Заодно фиксирует контракт «staging не требует связи с VK»: заливка в VK
/// живёт внутри `POST /messages`, а не здесь (upload-сервер выдаётся под
/// конкретный `peer_id`, который на этом шаге ещё неизвестен).
#[tokio::test]
async fn test_upload_stages_unicode_name_end_to_end() {
    let h = start_harness_with(Some("vk1.a.TEST")).await;
    authorize(&h.state).await;
    // Связи нет — staging обязан пройти всё равно.
    h.state
        .note_down(forge_vk::dto::now_unix(), "connection reset")
        .await;

    // encodeURIComponent("фото 🌍pic.jpg")
    let resp = reqwest::Client::new()
        .post(format!("{}/api/vk/upload", h.base()))
        .header("content-type", "application/octet-stream")
        .header(
            "x-forge-file-name",
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

    forge_vk::shutdown(&h.state).await;
}

/// `upload_id` тратится РОВНО ОДИН РАЗ.
///
/// Двойной клик по «Отправить» и автоповтор фронта частотнее неудачной
/// заливки, а цена ошибки разная: лишнее сообщение у собеседника не
/// отзывается, а файл всегда можно залить заново. Поэтому запись реестра
/// потребляется ДО обращения к VK, и повтор получает `409 invalid_state`
/// независимо от того, чем закончилась первая попытка.
#[tokio::test]
async fn test_upload_id_is_consumed_exactly_once() {
    let (h, stub) = start_authorized_with_stub().await;
    // Первая отправка споткнётся на выдаче upload-сервера — но запись реестра
    // к этому моменту уже потрачена.
    stub.reply_error("photos.getMessagesUploadServer", 1, json!({}));

    let client = reqwest::Client::new();
    let staged: Value = client
        .post(format!("{}/api/vk/upload", h.base()))
        .header("content-type", "application/octet-stream")
        .header("x-forge-file-name", "pic.jpg")
        .body(&b"abc123"[..])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let upload_id = staged["upload_id"].as_str().unwrap().to_string();

    let first = client
        .post(format!("{}/api/vk/dialogs/1234/messages", h.base()))
        .json(&json!({"upload_ids": [upload_id]}))
        .send()
        .await
        .unwrap();
    assert_ne!(
        first.status(),
        200,
        "стенд отвечает ошибкой VK — успех означал бы, что конвейер не звался"
    );
    assert!(
        h.state.upload_entry(&upload_id).await.is_none(),
        "запись обязана быть потреблена даже при неудачной заливке"
    );

    let second = client
        .post(format!("{}/api/vk/dialogs/1234/messages", h.base()))
        .json(&json!({"upload_ids": [upload_id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
    assert_eq!(
        second.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_state"
    );

    forge_vk::shutdown(&h.state).await;
}

/// Пустое сообщение без вложений — `400 invalid_input`, и до VK оно не
/// доезжает.
#[tokio::test]
async fn test_empty_message_never_reaches_vk() {
    let (h, stub) = start_authorized_with_stub().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/vk/dialogs/1234/messages", h.base()))
        .json(&json!({"text": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_input"
    );
    assert_eq!(
        stub.count("messages.send"),
        0,
        "пустое сообщение не имеет права уехать: {:?}",
        stub.calls()
    );

    forge_vk::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// WebSocket.
// ---------------------------------------------------------------------------

/// Первый кадр WS — `login_state`, ДО любых событий.
///
/// Клиенту не нужно ждать события, чтобы отрисовать состояние логина; на этом
/// же держится «панель показывает форму входа сразу после подключения».
#[tokio::test]
async fn test_ws_first_frame_is_login_state() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .expect("WS-апгрейд обязан пройти");

    let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("первый кадр не пришёл")
        .expect("поток закрылся")
        .expect("ошибка чтения");
    let Message::Text(text) = frame else {
        panic!("ожидался текстовый кадр");
    };
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "login_state");
    assert_eq!(v["state"], "not_configured");

    forge_vk::shutdown(&h.state).await;
}

/// Событие из broadcast доезжает до клиента, а битый входящий кадр НЕ рвёт
/// соединение (он может прийти от более новой версии фронта или из кэша PWA).
#[tokio::test]
async fn test_ws_delivers_events_and_survives_garbage() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();

    // Съедаем первый кадр (login_state).
    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("нет первого кадра");

    // Мусор от клиента: не-JSON, неизвестный тип кадра и бинарь.
    ws.send(Message::Text("не json вовсе".into())).await.unwrap();
    ws.send(Message::Text(r#"{"type":"from_the_future"}"#.into()))
        .await
        .unwrap();
    ws.send(Message::Binary(vec![0xff, 0xfe, 0x00, 0x9f]))
        .await
        .unwrap();

    h.state.broadcast_event(VkServerMsg::UnreadTotal { count: 7 });

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut seen = false;
    while std::time::Instant::now() < deadline && !seen {
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("нет кадров — соединение мертво")
            .expect("поток закрылся")
            .expect("ошибка чтения");
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "unread_total" {
                assert_eq!(v["count"], 7);
                seen = true;
            }
        }
    }
    assert!(seen, "кадр unread_total не доехал");

    forge_vk::shutdown(&h.state).await;
}

/// Молчащий клиент отключается по idle-таймауту.
///
/// Тайминги укорочены до сотен миллисекунд (`VkState::with_ws_timings`),
/// поэтому проверяется НАСТОЯЩИЙ цикл обработчика, а не его пересказ. Клиент
/// получает ping'и, но не отвечает `pong` — сервер обязан закрыть соединение.
#[tokio::test]
async fn test_ws_closes_idle_connection_without_pong() {
    let h = start_harness_opts(Opts {
        ws: Some((Duration::from_millis(120), Duration::from_millis(400))),
        ..Opts::default()
    })
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let mut saw_ping = false;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = ws.next().await {
            match frame {
                Ok(Message::Text(t)) => {
                    let kind = serde_json::from_str::<Value>(&t)
                        .ok()
                        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string));
                    if kind.as_deref() == Some("ping") {
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

    forge_vk::shutdown(&h.state).await;
}

/// Клиент, который отвечает `pong`, живёт дольше idle-таймаута.
///
/// Обратная сторона предыдущего теста: без неё «закрываем всех подряд» тоже
/// прошло бы проверку.
#[tokio::test]
async fn test_ws_keeps_alive_while_client_ponging() {
    let idle = Duration::from_millis(400);
    let h = start_harness_opts(Opts {
        ws: Some((Duration::from_millis(120), idle)),
        ..Opts::default()
    })
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();

    // Пропускаем снапшот login_state.
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap();

    let until = std::time::Instant::now() + idle * 3;
    while std::time::Instant::now() < until {
        ws.send(Message::Text(r#"{"type":"pong"}"#.to_string()))
            .await
            .expect("соединение закрылось, хотя клиент отвечает");
        tokio::time::sleep(idle / 4).await;
    }

    // Соединение живо: событие из broadcast всё ещё доезжает.
    h.state.broadcast_event(VkServerMsg::UnreadTotal { count: 3 });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut saw = false;
    while std::time::Instant::now() < deadline && !saw {
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("нет кадров — соединение мертво")
            .expect("поток закрылся")
            .expect("ошибка чтения");
        if let Message::Text(t) = frame {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "unread_total" {
                saw = true;
            }
        }
    }
    assert!(saw, "живое соединение обязано доставлять события");

    forge_vk::shutdown(&h.state).await;
}

/// Отставший подписчик получает `resync`, а соединение живёт дальше.
///
/// Синхронный цикл `broadcast_event` без единого await гарантированно
/// переполняет канал (ёмкость [`forge_vk::state::BROADCAST_CAPACITY`]) раньше,
/// чем обработчик успеет прочитать хоть одно событие. Это точная модель
/// реального шторма событий LongPoll при медленной вкладке: рвать соединение
/// нельзя (переподключение стоит нового `getLongPollServer`), поэтому клиенту
/// говорят «перечитай состояние сам».
#[tokio::test]
async fn test_ws_lagged_client_gets_resync_and_survives() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();
    // Снапшот login_state: обработчик дошёл до цикла select.
    let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();

    for _ in 0..(forge_vk::state::BROADCAST_CAPACITY + 500) {
        h.state.broadcast_event(VkServerMsg::Ping);
    }
    // Маркер после переполнения: он обязан доехать ПОСЛЕ resync.
    h.state.broadcast_event(VkServerMsg::UnreadTotal { count: 42 });

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
                Some("unread_total") if v["count"] == 42 => {
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

    forge_vk::shutdown(&h.state).await;
}

/// Апгрейд не ломается от query-параметров: и с токеном + неизвестными
/// полями, и с пустым `token=`.
///
/// Реальный случай — реконнект-URL фронта (`withWsToken`), куда позже допишут
/// параметры; сервер обязан их игнорировать, а не отвечать 400.
#[tokio::test]
async fn test_ws_upgrade_tolerates_token_and_unknown_query() {
    let h = start_harness().await;
    for query in ["?token=abc&v=2", "?token=", "?tab=7&from=pwa"] {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws/vk{query}", h.addr))
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

    forge_vk::shutdown(&h.state).await;
}

/// Один broadcast — все вкладки. На этом держится бейдж непрочитанного в
/// нескольких окнах.
#[tokio::test]
async fn test_ws_broadcast_reaches_multiple_clients() {
    let h = start_harness().await;
    let (mut ws1, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();
    let (mut ws2, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();
    for ws in [&mut ws1, &mut ws2] {
        let _ = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap();
    }

    h.state.broadcast_event(VkServerMsg::Read {
        dialog_id: "1234".into(),
        max_id: 7001,
        direction: "out".into(),
    });
    for (n, ws) in [&mut ws1, &mut ws2].into_iter().enumerate() {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("событие не доехало до подписчика {n}"))
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        assert_eq!(v["type"], "read");
        assert_eq!(v["dialog_id"], "1234");
    }

    forge_vk::shutdown(&h.state).await;
}

/// Shutdown хоста закрывает WS-соединения, а не оставляет их висеть.
#[tokio::test]
async fn test_ws_shutdown_closes_client_promptly() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/vk", h.addr))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next()).await;

    let started = std::time::Instant::now();
    forge_vk::shutdown(&h.state).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut closed = false;
    while std::time::Instant::now() < deadline && !closed {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(None) | Err(_) => closed = true,
            Ok(Some(Ok(Message::Close(_)))) => closed = true,
            Ok(Some(Err(_))) => closed = true,
            Ok(Some(Ok(_))) => {}
        }
    }
    assert!(closed, "соединение обязано закрыться по shutdown");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "закрытие затянулось: {:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// Lifecycle и конфиг.
// ---------------------------------------------------------------------------

/// Lifecycle плагина в хосте: `spawn_workers` не виснет, `shutdown`
/// идемпотентен и завершается быстро (иначе Ctrl-C у devforge подвисал бы).
#[tokio::test]
async fn test_workers_start_and_stop_cleanly() {
    let h = start_harness().await;
    forge_vk::spawn_workers(&h.state).await;

    let stopped = tokio::time::timeout(Duration::from_secs(5), forge_vk::shutdown(&h.state)).await;
    assert!(stopped.is_ok(), "shutdown не уложился в 5с");
    assert!(h.state.shutdown.is_cancelled());

    // Повторный вызов из второго сигнала (Ctrl-C + SIGTERM от systemd).
    let again = tokio::time::timeout(Duration::from_secs(5), forge_vk::shutdown(&h.state)).await;
    assert!(again.is_ok(), "повторный shutdown завис");
}

/// Битый `vk.toml` НЕ роняет плагин: конфиг чинится дефолтами, `init`
/// проходит, роуты поднимаются.
///
/// VK — опциональная вкладка, и сломанный конфиг не должен мешать
/// пользователю работать с терминалом. Изоляция полная: `FORGE_VK_CONFIG_PATH`
/// указывает в tempdir, иначе `load()` читал бы реальный
/// `~/.config/forge/vk.toml` разработчика.
#[tokio::test]
async fn test_broken_config_does_not_break_init() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("nested/vk.toml");
    let cache = dir.path().join("nested/cache");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "это [не toml вовсе\naccess_token = ").unwrap();

    let saved_config = std::env::var("FORGE_VK_CONFIG_PATH").ok();
    let saved_cache = std::env::var("FORGE_VK_CACHE_DIR").ok();
    std::env::set_var("FORGE_VK_CONFIG_PATH", &config);
    std::env::set_var("FORGE_VK_CACHE_DIR", &cache);

    let cfg = VkConfig::load();
    assert_eq!(cfg.config_path, config, "читаем и пишем один и тот же файл");
    assert_eq!(cfg.cache_dir, cache);
    assert_eq!(cfg.api_version, forge_vk::config::DEFAULT_API_VERSION);
    assert_eq!(
        cfg.rate_limit_rps,
        forge_vk::config::DEFAULT_RATE_LIMIT_RPS
    );
    assert!(!cfg.has_token(), "битый файл не имеет права дать токен");

    let state = forge_vk::init(cfg).expect("init обязан пережить битый конфиг");
    assert!(cache.is_dir(), "каталог кэша создаётся при init");
    let app: Router = forge_vk::register_routes(Router::new(), state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let body: Value = reqwest::get(format!("http://{addr}/api/vk/status"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "not_configured");

    match saved_config {
        Some(v) => std::env::set_var("FORGE_VK_CONFIG_PATH", v),
        None => std::env::remove_var("FORGE_VK_CONFIG_PATH"),
    }
    match saved_cache {
        Some(v) => std::env::set_var("FORGE_VK_CACHE_DIR", v),
        None => std::env::remove_var("FORGE_VK_CACHE_DIR"),
    }

    forge_vk::shutdown(&state).await;
}
