//! Смоук-тесты плагина Bitrix24 ПО ПРОВОДУ (близнец `vk_smoke.rs`).
//!
//! Отличие от юнит-тестов внутри `plugins/bitrix/src/**`: здесь поднимается
//! НАСТОЯЩИЙ axum-сервер на свободном порту и запросы идут через реальный
//! HTTP-клиент. Это ловит ровно тот класс дефектов, который unit-тесты с
//! `oneshot` пропускают: несмонтированный роут, конфликт шаблонов путей,
//! сломанный WS-апгрейд, порядок слоёв.
//!
//! # Что покрыто в Фазе 1
//!
//! **Opt-in.** [`test_routes_absent_without_the_plugin`] — ГЛАВНЫЙ инвариант
//! фичи: собираем роутер БЕЗ `register_routes` и убеждаемся, что весь префикс
//! `/api/bitrix/*` и `/ws/bitrix` отвечают 404. Именно на этом 404 держится
//! гейт кнопки вкладки во фронте, поэтому проверка не косметическая:
//! подмонтируй кто-нибудь `/api/bitrix/status` мимо флага — вкладка появилась
//! бы у всех, кто запускает devforge без `--bitrix`.
//!
//! **Статус.** 200 в любом состоянии автомата плюс форма ответа
//! (`enabled`, `login`, `max_upload_bytes`, `rate_limit_rps`,
//! `capabilities`, `hint`).
//!
//! **Секрет.** `/status` не содержит ни кода вебхука, ни хоста портала, ни
//! слова `fingerprint` — во ВСЕХ восьми состояниях подключения. Проверка
//! идёт по сырому телу ответа, а не по разобранным полям: утечка через
//! новое поле обязана краснеть здесь автоматически.
//!
//! **Жизненный цикл.** `init` синхронный и без сети (`base_url_override` на
//! заведомо закрытый порт 1, порог 500 мс); `shutdown` идемпотентна (три
//! вызова подряд, третий — с уже пустым реестром воркеров).
//!
//! **WS.** Апгрейд `/ws/bitrix`, первый кадр `login_state`, ping/pong живёт
//! дольше idle-таймаута (тайминги укорочены через
//! `B24State::with_ws_timings`, иначе тест ждал бы боевую минуту),
//! broadcast доезжает до клиента, битый входящий кадр НЕ рвёт соединение,
//! закрытие по shutdown.
//!
//! **Конфиг.** Битый `bitrix.toml` не роняет `init` — плагин поднимается на
//! дефолтах.
//!
//! Тесты НЕ требуют сети и НЕ требуют вебхука. Пути конфига задаются прямо в
//! [`BitrixConfig`], поэтому домашний каталог пользователя не затрагивается —
//! в частности, живой `~/.config/forge/bitrix.toml` разработчика не читается
//! и не перезаписывается.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

/// Портал и код тестового вебхука. Значения намеренно узнаваемые: тест ищет
/// их подстрокой в сыром теле ответа.
const PORTAL: &str = "test-work-2.bitrix24.ru";
const CODE: &str = "wqdzbfhmn83d23j4";

fn webhook() -> WebhookUrl {
    WebhookUrl::new(format!("https://{PORTAL}/rest/1/{CODE}/"))
}

// ---------------------------------------------------------------------------
// Стенд плагина.
// ---------------------------------------------------------------------------

/// Живой сервер с примонтированным плагином.
struct Harness {
    addr: SocketAddr,
    state: Arc<B24State>,
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
/// `start(None, None)` на месте вызова не читался бы вовсе.
#[derive(Default)]
struct Opts {
    /// Класть ли вебхук в конфиг. Единственный способ достичь состояния
    /// `disconnected` без сети: сетевой проверки стенд не делает.
    webhook: Option<WebhookUrl>,
    /// Укороченные `(heartbeat, idle)` для WS-тестов.
    ws: Option<(Duration, Duration)>,
}

/// Поднимает плагин на свободном порту. Конфиг полностью изолирован во
/// временном каталоге (никаких обращений к `~/.config/forge`).
async fn start_harness() -> Harness {
    start_harness_opts(Opts::default()).await
}

/// Общий конструктор стенда.
async fn start_harness_opts(opts: Opts) -> Harness {
    let tempdir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        cache_dir: tempdir.path().join("cache"),
        config_path: tempdir.path().join("bitrix.toml"),
        // Заведомо недостижимый адрес: если какой-нибудь воркер решит сходить
        // в сеть на старте, он упрётся в connect-refused локально, а не
        // постучится в настоящий портал из CI.
        //
        // Схема именно `https`, хотя стенда за адресом нет: правило Р17 (Ф2)
        // разрешает `http://127.0.0.1` только при непустом
        // `FORGE_BITRIX_CONFIG_PATH`, а подменять переменные процесса в
        // многопоточном тестовом бинаре — гонка на ровном месте. Изоляция
        // конфига здесь и так полная: `config_path` указывает во временный
        // каталог.
        base_url_override: "https://127.0.0.1:1/rest/1/stub".to_string(),
        webhook: opts.webhook.unwrap_or_default(),
        ..BitrixConfig::default()
    };

    let state = match opts.ws {
        // `init` сам создаёт состояние боевым конструктором, поэтому для
        // укороченных таймингов состояние собирается явно. Валидация конфига
        // при этом не теряется — она вызывается тут же.
        Some((hb, idle)) => {
            cfg.validate().expect("конфиг стенда обязан быть валидным");
            let _ = cfg.ensure_dirs();
            B24State::with_ws_timings(cfg, hb, idle)
        }
        None => forge_bitrix::init(cfg).expect("init must succeed on a valid config"),
    };
    let app: Router = forge_bitrix::register_routes(Router::new(), state.clone());

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

/// Все восемь состояний автомата подключения — источник истины для тестов
/// утечки секрета. Ни одно из них не требует сети.
fn all_login_states() -> Vec<LoginFlow> {
    vec![
        LoginFlow::NotConfigured,
        LoginFlow::Checking,
        LoginFlow::InvalidUrl {
            error: Some("это не похоже на адрес входящего вебхука".into()),
        },
        LoginFlow::InvalidCredentials {
            error: Some("портал отверг учётные данные".into()),
        },
        LoginFlow::ScopeMissing {
            error: Some("у вебхука нет права im".into()),
        },
        LoginFlow::Throttled {
            retry_after_secs: Some(7),
        },
        LoginFlow::Disconnected {
            error: Some("connect timeout".into()),
        },
        LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                name: Some("Тест".into()),
                last_name: Some("Тестов".into()),
                admin: true,
            },
            portal: PORTAL.into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// Opt-in: без плагина роутов нет.
// ---------------------------------------------------------------------------

/// ГЛАВНЫЙ ИНВАРИАНТ ФИЧИ.
///
/// Сервер собран БЕЗ `forge_bitrix::register_routes` — ровно так выглядит
/// `devforge run` без флага `--bitrix`. Весь префикс `/api/bitrix/*` и
/// WS-роут обязаны отвечать **404**, а не 401, не 500 и не 200.
///
/// Почему именно 404 и почему это не косметика: гейт кнопки вкладки во фронте
/// (`js/bitrix/main.js::initB24Gate`) считает 404 сигналом «плагин выключен» и
/// молча оставляет кнопку скрытой. Любой другой код там означал бы «плагин
/// есть, но сломан», и вкладка появилась бы у всех, кто запускает devforge без
/// флага.
///
/// Тест обязан упасть, если кто-нибудь снимет условие регистрации в
/// `tmux-web/src/main.rs`.
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

    // Весь контракт путей из шапки `plugins/bitrix/src/routes/mod.rs` —
    // включая те, что появятся в следующих фазах: если однажды роут
    // смонтируют мимо гейта, тест обязан покраснеть сразу, а не в той фазе,
    // где кто-то вспомнит дописать сюда строчку.
    for path in [
        "/api/bitrix/status",
        "/api/bitrix/filters",
        "/api/bitrix/dialogs",
        "/api/bitrix/dialogs/chat7/messages",
        "/api/bitrix/messages/113",
        "/api/bitrix/tasks",
        "/api/bitrix/tasks/counters",
        "/api/bitrix/tasks/3",
        "/api/bitrix/media/avatar/1",
        "/api/bitrix/media/file/chat7/63/0",
        "/api/bitrix/upload",
        "/api/bitrix/login/webhook",
        "/api/bitrix/logout",
        "/api/bitrix/dialogs/chat7/read",
        "/api/bitrix/dialogs/chat7/unread",
        "/api/bitrix/dialogs/chat7/typing",
        "/api/bitrix/dialogs/chat7/pin",
        "/api/bitrix/dialogs/chat7/hide",
        "/api/bitrix/messages/113/like",
    ] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(
            resp.status(),
            404,
            "{path} обязан отсутствовать без регистрации плагина"
        );
    }

    // WS-апгрейд тоже не проходит.
    let ws = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/bitrix")).await;
    assert!(ws.is_err(), "/ws/bitrix не должен существовать без плагина");
}

/// ВТОРАЯ ПОЛОВИНА ГЕЙТА — та, до которой предыдущий тест не дотягивается.
///
/// [`test_routes_absent_without_the_plugin`] собирает роутер сам и потому
/// доказывает лишь, что плагин не монтируется без вызова `register_routes`.
/// Но реальный гейт живёт в `tmux-web/src/main.rs`, а он — бинарь: поднять
/// его функции из интеграционного теста нельзя. Поэтому условие проверяется
/// ПО ИСХОДНИКУ, как и страж «секрет не в логе» в плагинах.
///
/// Три инварианта разом:
///
/// 1. `register_routes` вызывается ровно один раз и стоит ВНУТРИ ветки
///    `if bitrix_enabled`. Снятие гейта (регистрация безусловно) роняет тест.
/// 2. Регистрация идёт СТРОГО ДО `.fallback(static_embed::serve_static)` —
///    иначе fallback перехватил бы `/api/bitrix/*` и роуты плагина отвечали
///    бы 404 при включённом флаге.
/// 3. Регистрация идёт СТРОГО ДО `auth::bearer_auth` и `auth::csrf_guard` —
///    иначе роуты плагина оказались бы ВНЕ авторизации в remote-mode. Это
///    самая дорогая из трёх ошибок: она не видна ни в одном функциональном
///    тесте, потому что всё «работает».
#[test]
fn test_the_host_registers_the_plugin_only_behind_the_flag() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("tmux-web/src/main.rs читается");

    // Комментарии выброшены НАМЕРЕННО: соседние блоки объясняют этот самый
    // порядок слоёв словами и содержат те же подстроки, поэтому поиск по
    // сырому тексту находил бы объяснение вместо кода и тест «доказывал» бы
    // порядок комментариев.
    let main_rs: String = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(
        main_rs.matches("forge_bitrix::register_routes").count(),
        1,
        "регистрация плагина обязана быть ровно в одном месте"
    );

    let gate = main_rs
        .find("if bitrix_enabled {")
        .expect("ветка `if bitrix_enabled {` не найдена — гейт снят?");
    let register = main_rs.find("forge_bitrix::register_routes").unwrap();
    let spawn = main_rs.find("forge_bitrix::spawn_workers").unwrap();
    assert!(
        gate < register && gate < spawn,
        "register_routes/spawn_workers обязаны стоять ВНУТРИ ветки `if bitrix_enabled`"
    );

    let fallback = main_rs
        .find(".fallback(static_embed::serve_static)")
        .expect("fallback статики не найден");
    assert!(
        register < fallback,
        "регистрация после .fallback даст 404 на роуты плагина"
    );

    for layer in ["auth::bearer_auth", "auth::csrf_guard"] {
        let at = main_rs
            .find(layer)
            .unwrap_or_else(|| panic!("слой {layer} не найден"));
        assert!(
            register < at,
            "регистрация после {layer} оставила бы /api/bitrix/* без авторизации"
        );
    }

    // Ошибка init не имеет права ронять сервер: у пользователя со сломанным
    // `bitrix.toml` обязан остаться терминал.
    let branch_end = main_rs[gate..]
        .find("forge-bitrix: init failed")
        .expect("ветка warn при ошибке init не найдена");
    assert!(
        !main_rs[gate..gate + branch_end].contains("forge_bitrix::init(forge_bitrix::BitrixConfig::load())?"),
        "init обязан обрабатываться match'ем с warn, а не `?`"
    );

    // Плагин гасится вместе с сервером — иначе курсор событий (Ф7) не
    // успевал бы записаться на диск.
    assert!(
        main_rs.contains("forge_bitrix::shutdown"),
        "graceful shutdown плагина не вызывается"
    );
}

// ---------------------------------------------------------------------------
// Статус.
// ---------------------------------------------------------------------------

/// Плагин зарегистрирован, вебхука нет: 200 и `not_configured`.
#[tokio::test]
async fn test_status_is_not_configured() {
    let h = start_harness().await;
    let resp = reqwest::get(format!("{}/api/bitrix/status", h.base()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["enabled"], true);
    assert_eq!(body["login"]["state"], "not_configured");
    assert!(body["login"]["user"].is_null());
    assert!(body["login"]["error"].is_null());
    // Фронтовый планировщик настраивается по этим полям, а не по своей копии
    // констант.
    assert_eq!(body["rate_limit_rps"], 2);
    assert_eq!(body["max_upload_bytes"], 25 * 1024 * 1024);
    // Честный список того, чего платформа не умеет (§6 плана): UI по нему
    // прячет элементы, а не показывает ошибки.
    assert_eq!(body["capabilities"]["tasks"], true);
    assert_eq!(body["capabilities"]["incoming_typing"], false);
    assert_eq!(body["capabilities"]["realtime_presence"], false);
    assert_eq!(body["capabilities"]["realtime_edits"], false);
    assert_eq!(body["capabilities"]["reactions"], false);
    // Подсказка «где взять вебхук» приходит с сервера: разметка живёт в кэше
    // PWA неделями, а интерфейс портала меняется.
    assert!(
        !body["hint"].as_str().unwrap_or_default().is_empty(),
        "форма подключения без подсказки бесполезна: {body}"
    );

    forge_bitrix::shutdown(&h.state).await;
}

/// С вебхуком стартовое состояние — `disconnected`: `init` синхронный и сети
/// не трогает, проверка вебхука (Ф3) делается фоново.
#[tokio::test]
async fn test_status_is_disconnected_with_a_webhook() {
    let h = start_harness_opts(Opts {
        webhook: Some(webhook()),
        ..Opts::default()
    })
    .await;
    let body: Value = reqwest::get(format!("{}/api/bitrix/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "disconnected");
    forge_bitrix::shutdown(&h.state).await;
}

/// ИНВАРИАНТ БЕЗОПАСНОСТИ: `/status` не отдаёт ни кода вебхука, ни хоста
/// портала, ни слова `fingerprint` — НИ В ОДНОМ состоянии автомата.
///
/// Проверка идёт по СЫРОМУ телу ответа, а не по разобранным полям: так утечка
/// через любое новое поле краснеет автоматически, без правки теста.
///
/// Почему хост тоже под запретом, хотя пользователь и так видит его в
/// адресной строке Bitrix24: `/api/bitrix/status` уходит в браузер, в
/// devtools и в HAR-файл, который человек прикладывает к обращению в
/// поддержку. Хост сужает перебор кода до одного портала.
#[tokio::test]
async fn test_status_never_leaks_the_webhook_in_any_state() {
    for flow in all_login_states() {
        let kind = flow.kind();
        let h = start_harness_opts(Opts {
            webhook: Some(webhook()),
            ..Opts::default()
        })
        .await;
        h.state.set_login(flow).await;

        let resp = reqwest::get(format!("{}/api/bitrix/status", h.base()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "состояние {kind}: /status обязан быть 200");
        let text = resp.text().await.unwrap();

        assert_no_secret_anywhere(&text, kind);
        // Состояние при этом ДОЕХАЛО — иначе тест был бы зелёным на пустом
        // ответе.
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["login"]["state"], kind);

        forge_bitrix::shutdown(&h.state).await;
    }
}

/// Сколько подряд идущих символов кода вебхука считаются утечкой.
///
/// Порог не косметический. Код `wqdzbfhmn83d23j4` состоит из 16 символов
/// алфавита `[A-Za-z0-9]`; каждый показанный символ убирает из перебора
/// множитель 62. Восьми символов достаточно, чтобы остаток стал перебираемым,
/// а «покажем начало кода, чтобы пользователь узнал свой вебхук» — ровно тот
/// соблазн, который этот страж и обязан ловить. Единственное законное
/// исключение — [`forge_bitrix::WebhookUrl::fingerprint`] с шестью последними
/// символами, и он живёт ТОЛЬКО в логах сервера, не в HTTP-ответах.
const MIN_LEAKED_PIECE: usize = 8;

/// Ни кода вебхука (даже куском), ни хоста портала, ни слова `fingerprint`.
///
/// Отдельной функцией, а не строчкой в тесте: тот же набор проверок нужен и
/// ответам роутов подключения, и снапшоту статуса, и одинаковость этих
/// проверок — часть контракта.
fn assert_no_secret_anywhere(text: &str, what: &str) {
    assert!(
        !text.contains(PORTAL),
        "хост портала в ответе ({what}): {text}"
    );
    assert!(
        !text.contains("fingerprint"),
        "отпечаток в ответе ({what}): {text}"
    );
    let chars: Vec<char> = CODE.chars().collect();
    assert!(chars.len() > MIN_LEAKED_PIECE, "код для проверки слишком короткий");
    for start in 0..=chars.len() - MIN_LEAKED_PIECE {
        let piece: String = chars[start..start + MIN_LEAKED_PIECE].iter().collect();
        assert!(
            !text.contains(&piece),
            "кусок кода вебхука «{piece}» в ответе ({what}): {text}"
        );
    }
}

/// СТРАЖ ФАЗЫ 3, вторая половина: страж выше проверяет ответы сервера, а этот
/// — что сам страж всё ещё умеет ловить утечку.
///
/// Без него `assert_no_secret_anywhere` мог бы тихо превратиться в
/// no-op (опечатка в константе, пустой цикл после рефакторинга), и весь набор
/// проверок «секрет не уезжает наружу» зеленел бы на любом теле ответа.
#[test]
fn test_the_secret_guard_actually_catches_a_leak() {
    // Целиком.
    assert!(std::panic::catch_unwind(|| {
        assert_no_secret_anywhere(&format!(r#"{{"webhook":"{CODE}"}}"#), "self-test");
    })
    .is_err());

    // Только ПОЛОВИНОЙ — ровно то, что выглядит «безобидным показом».
    let half: String = CODE.chars().take(MIN_LEAKED_PIECE).collect();
    assert!(std::panic::catch_unwind(|| {
        assert_no_secret_anywhere(&format!(r#"{{"hint":"…{half}…"}}"#), "self-test");
    })
    .is_err());

    // Хост портала.
    assert!(std::panic::catch_unwind(|| {
        assert_no_secret_anywhere(&format!(r#"{{"portal":"{PORTAL}"}}"#), "self-test");
    })
    .is_err());

    // А честный ответ проходит: страж не должен запрещать вообще всё.
    assert_no_secret_anywhere(r#"{"login":{"state":"authorized","user":{"id":"1"}}}"#, "self-test");
    // Шесть последних символов кода — это отпечаток, и он законен в логах;
    // порог стража его не задевает.
    let tail: String = CODE.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    assert_no_secret_anywhere(&format!(r#"{{"log":"…{tail}"}}"#), "self-test");
}

/// `throttled` (Р16) доносит до фронта время ожидания, а `authorized` —
/// профиль владельца вебхука. Оба поля нужны экрану подключения и оба
/// секретом не являются.
#[tokio::test]
async fn test_status_carries_retry_after_and_user() {
    let h = start_harness().await;

    h.state
        .set_login(LoginFlow::Throttled {
            retry_after_secs: Some(42),
        })
        .await;
    let body: Value = reqwest::get(format!("{}/api/bitrix/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "throttled");
    assert_eq!(body["login"]["retry_after_secs"], 42);

    h.state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                name: Some("Тест".into()),
                last_name: Some("Тестов".into()),
                admin: true,
            },
            portal: PORTAL.into(),
        })
        .await;
    let body: Value = reqwest::get(format!("{}/api/bitrix/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "authorized");
    assert_eq!(body["login"]["user"]["id"], "1");
    assert_eq!(body["login"]["user"]["admin"], true);
    // Необязательное поле не сериализуется, когда его нет: старый фронт-разбор
    // продолжает работать без правок.
    assert!(body["login"].get("retry_after_secs").is_none(), "{body}");

    forge_bitrix::shutdown(&h.state).await;
}

/// Тест фиксирует СОСТАВ реализованного, чтобы случайно смонтированный
/// полуготовый роут не уехал в релиз, а недостающий — не выглядел как
/// выключенный плагин (404 снаружи означает ровно это и гасит кнопку вкладки).
#[tokio::test]
async fn test_only_the_implemented_routes_are_mounted() {
    let h = start_harness().await;
    // Данные (Ф4), лента (Ф5) и задачи (Ф10) смонтированы: без вебхука они
    // отвечают 409 «портал не подключён» — ПЕРВОЙ ступенью лестницы, ещё до
    // разбора параметров и до разбора `:id`. Именно 409, а не 404: 404
    // означает «плагина нет» и гасит кнопку вкладки.
    for path in [
        "/api/bitrix/dialogs",
        "/api/bitrix/filters",
        "/api/bitrix/dialogs/chat7/messages",
        "/api/bitrix/messages/113",
        "/api/bitrix/tasks",
        "/api/bitrix/tasks/counters",
        "/api/bitrix/tasks/3",
    ] {
        let resp = reqwest::get(format!("{}{path}", h.base())).await.unwrap();
        assert_eq!(resp.status(), 409, "{path} обязан отвечать 409 not_authorized");
    }
    // Роуты подключения (Ф3) и действий над диалогом (Ф6) смонтированы как
    // POST: GET по ним даёт 405 «метод не тот», а не 404 «пути нет». Разница
    // принципиальна — гейт кнопки вкладки во фронте держится именно на 404.
    for path in [
        "/api/bitrix/login/webhook",
        "/api/bitrix/logout",
        "/api/bitrix/dialogs/chat7/read",
        "/api/bitrix/dialogs/chat7/unread",
        "/api/bitrix/dialogs/chat7/typing",
        "/api/bitrix/dialogs/chat7/pin",
        "/api/bitrix/dialogs/chat7/hide",
        "/api/bitrix/messages/113/like",
        // Приём файла (Ф9) — тоже POST-only: 405 на GET, а не 404. Разница
        // принципиальна ровно по той же причине, что и выше.
        "/api/bitrix/upload",
        // Действие над задачей (Ф10).
        "/api/bitrix/tasks/3/start",
    ] {
        let resp = reqwest::get(format!("{}{path}", h.base())).await.unwrap();
        assert_eq!(resp.status(), 405, "{path} обязан быть смонтирован как POST");
    }
    forge_bitrix::shutdown(&h.state).await;
}

/// Подключение и выход ПО ПРОВОДУ: тела ответов не содержат секрета, а
/// мусорный ввод отвергается 400 без единого сетевого вызова.
///
/// Сетевой части здесь нет вовсе — `base_url_override` смотрит на закрытый
/// порт 1, поэтому проверяется ровно то, что можно проверить без портала:
/// контракт HTTP и отсутствие утечки. Полный путь `profile` → `scope` живёт на
/// стенде (`plugins/bitrix/tests/b24_stub.rs`).
#[tokio::test]
async fn test_login_routes_reject_garbage_and_never_leak() {
    let h = start_harness().await;
    let client = reqwest::Client::new();

    for raw in ["", "не урл", "https://test-work-2.bitrix24.ru/rest/1/"] {
        let resp = client
            .post(format!("{}/api/bitrix/login/webhook", h.base()))
            .json(&serde_json::json!({ "webhook": raw }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "вход {raw:?} обязан быть отвергнут");
        let text = resp.text().await.unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["code"], "invalid_input");
        assert_no_secret_anywhere(&text, "login/invalid");
    }

    // Состояние автомата изменилось вместе с ответом: соседние вкладки узнают
    // об отказе только кадром `login_state`.
    let body: Value = reqwest::get(format!("{}/api/bitrix/status", h.base()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["login"]["state"], "invalid_url");

    // Выход идемпотентен и тоже ничего не раскрывает.
    for _ in 0..2 {
        let resp = client
            .post(format!("{}/api/bitrix/logout", h.base()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let text = resp.text().await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["state"],
            "not_configured"
        );
        assert_no_secret_anywhere(&text, "logout");
    }

    forge_bitrix::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Жизненный цикл.
// ---------------------------------------------------------------------------

/// `init` синхронный и БЕЗ СЕТИ.
///
/// `base_url_override` смотрит на порт 1 (закрыт всегда): если бы `init`
/// ходил в портал «чтобы сразу показать имя пользователя», он упёрся бы в
/// connect-таймаут и не уложился в порог. Порог 500 мс выбран с большим
/// запасом относительно реальной работы (чтение конфига + mkdir).
#[tokio::test]
async fn test_init_is_synchronous_and_offline() {
    let tempdir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        cache_dir: tempdir.path().join("cache"),
        config_path: tempdir.path().join("bitrix.toml"),
        // `https` — по тем же причинам, что и в `start_harness_opts` (Р17).
        base_url_override: "https://127.0.0.1:1/rest/1/stub".into(),
        webhook: webhook(),
        ..BitrixConfig::default()
    };

    let started = std::time::Instant::now();
    let state = forge_bitrix::init(cfg).expect("init на валидном конфиге обязан пройти");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "init занял {elapsed:?} — он не должен ждать сеть"
    );
    assert_eq!(state.login_state_dto().await.state, "disconnected");

    // И `spawn_workers` тоже возвращается немедленно: хост зовёт его ДО
    // биндинга порта, и медленная сеть не имеет права задерживать старт.
    let started = std::time::Instant::now();
    forge_bitrix::spawn_workers(&state).await;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "spawn_workers обязан возвращаться сразу"
    );

    forge_bitrix::shutdown(&state).await;
}

/// `shutdown` идемпотентна: хост зовёт её из обработчика сигнала, а тот может
/// сработать дважды (Ctrl-C и следом SIGTERM от systemd).
///
/// Первый вызов гасит зарегистрированного воркера, второй и третий работают с
/// уже пустым реестром — и обязаны не паниковать и не виснуть.
#[tokio::test]
async fn test_shutdown_is_idempotent() {
    let h = start_harness().await;

    let token = h.state.shutdown.clone();
    h.state
        .register_worker(tokio::spawn(async move { token.cancelled().await }))
        .await;
    assert!(!h.state.shutdown.is_cancelled());

    forge_bitrix::shutdown(&h.state).await;
    assert!(h.state.shutdown.is_cancelled());
    assert!(h.state.workers.lock().await.is_empty());

    for attempt in 2..=3 {
        tokio::time::timeout(Duration::from_secs(2), forge_bitrix::shutdown(&h.state))
            .await
            .unwrap_or_else(|_| panic!("shutdown #{attempt} завис"));
    }
}

/// Битый `bitrix.toml` НЕ роняет плагин: конфиг чинится дефолтами.
///
/// Хост в этом случае обязан стартовать (у него `warn`, а не `?`), поэтому
/// пользователь с опечаткой в конфиге теряет вкладку, а не терминал.
#[tokio::test]
async fn test_a_broken_config_does_not_break_init() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("bitrix.toml");
    std::fs::write(&path, "this is ] not [ toml").unwrap();

    let cfg = BitrixConfig {
        cache_dir: tempdir.path().join("cache"),
        ..BitrixConfig::load_from_file(&path)
    };
    let state = forge_bitrix::init(cfg).expect("битый конфиг не должен ронять init");
    assert_eq!(state.login_state_dto().await.state, "not_configured");
    assert_eq!(state.config.rate_limit_rps, 2);
    forge_bitrix::shutdown(&state).await;
}

// ---------------------------------------------------------------------------
// WebSocket.
// ---------------------------------------------------------------------------

/// Первый кадр после апгрейда — `login_state`: клиенту не нужно ждать
/// события, чтобы отрисовать экран подключения.
#[tokio::test]
async fn test_ws_first_frame_is_login_state() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/bitrix", h.addr))
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

    forge_bitrix::shutdown(&h.state).await;
}

/// Событие из broadcast доезжает до клиента, а битый входящий кадр НЕ рвёт
/// соединение (он может прийти от более новой версии фронта или из кэша PWA).
#[tokio::test]
async fn test_ws_delivers_events_and_survives_garbage() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/bitrix", h.addr))
        .await
        .unwrap();

    // Съедаем первый кадр (login_state).
    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("нет первого кадра");

    // Мусор от клиента: сервер обязан его проигнорировать.
    ws.send(Message::Text("{\"type\":\"who-knows\"}".into()))
        .await
        .unwrap();
    ws.send(Message::Text("not json at all".into()))
        .await
        .unwrap();

    // Переход состояния — событие в broadcast.
    h.state.set_login(LoginFlow::Checking).await;

    let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("кадр не пришёл — мусор порвал соединение")
        .expect("поток закрылся")
        .expect("ошибка чтения");
    let Message::Text(text) = frame else {
        panic!("ожидался текстовый кадр");
    };
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "login_state");
    assert_eq!(v["state"], "checking");

    forge_bitrix::shutdown(&h.state).await;
}

/// Ping/pong держит соединение живым ДОЛЬШЕ idle-таймаута.
///
/// Тайминги укорочены (`heartbeat 150 мс`, `idle 500 мс`) — с боевыми 15/60
/// секунд этот тест шёл бы больше минуты. Проверяется именно то, ради чего
/// heartbeat и существует: сервер шлёт `ping`, клиент отвечает `pong`, и
/// соединение переживает несколько idle-окон подряд.
#[tokio::test]
async fn test_ws_ping_pong_outlives_the_idle_timeout() {
    let h = start_harness_opts(Opts {
        ws: Some((Duration::from_millis(150), Duration::from_millis(500))),
        ..Opts::default()
    })
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/bitrix", h.addr))
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_millis(1500);
    let mut pings = 0;
    while std::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(600), ws.next()).await;
        let Ok(Some(Ok(Message::Text(text)))) = next else {
            panic!("соединение умерло раньше срока (получено ping'ов: {pings})");
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        if v["type"] == "ping" {
            pings += 1;
            ws.send(Message::Text("{\"type\":\"pong\"}".into()))
                .await
                .expect("pong не ушёл — соединение уже закрыто");
        }
    }
    assert!(
        pings >= 3,
        "за 1.5с при heartbeat 150мс ожидались хотя бы 3 ping'а, пришло {pings}"
    );

    // Соединение всё ещё живо: idle-таймаут (500 мс) многократно перекрыт.
    h.state.set_login(LoginFlow::Checking).await;
    let frame = tokio::time::timeout(Duration::from_millis(600), ws.next())
        .await
        .expect("соединение закрылось по idle несмотря на pong'и")
        .expect("поток закрылся")
        .expect("ошибка чтения");
    assert!(matches!(frame, Message::Text(_)));

    forge_bitrix::shutdown(&h.state).await;
}

/// Молчащий клиент закрывается по idle-таймауту — иначе повисшие вкладки
/// копили бы сокеты на сервере до конца процесса.
#[tokio::test]
async fn test_ws_closes_a_silent_client_by_idle() {
    let h = start_harness_opts(Opts {
        // Heartbeat заведомо длиннее idle: сервер не успеет прислать ни
        // одного ping'а, и закрытие произойдёт именно по тишине клиента.
        ws: Some((Duration::from_secs(30), Duration::from_millis(200))),
        ..Opts::default()
    })
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/bitrix", h.addr))
        .await
        .unwrap();

    // login_state.
    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("нет первого кадра");

    // Клиент молчит — сервер обязан закрыть соединение сам.
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => return true,
                _ => continue,
            }
        }
        true
    })
    .await
    .expect("сервер не закрыл молчащее соединение");
    assert!(closed);

    forge_bitrix::shutdown(&h.state).await;
}

/// `shutdown` закрывает живые WS-соединения, а не оставляет их висеть до
/// таймаута: graceful shutdown хоста ждёт закрытия соединений axum'ом.
#[tokio::test]
async fn test_ws_closes_on_shutdown() {
    let h = start_harness().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/bitrix", h.addr))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("нет первого кадра");

    forge_bitrix::shutdown(&h.state).await;

    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => return true,
                _ => continue,
            }
        }
        true
    })
    .await
    .expect("соединение не закрылось после shutdown");
    assert!(closed);
}

/// Несколько вкладок получают одно и то же событие: broadcast идёт БЕЗ
/// фильтрации, потому что бейдж непрочитанного обязан работать из любой
/// вкладки devforge.
#[tokio::test]
async fn test_ws_broadcasts_to_every_client() {
    let h = start_harness().await;
    let url = format!("ws://{}/ws/bitrix", h.addr);
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    for ws in [&mut a, &mut b] {
        let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("нет первого кадра");
    }

    h.state.set_login(LoginFlow::Checking).await;

    for (name, ws) in [("A", &mut a), ("B", &mut b)] {
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .unwrap_or_else(|_| panic!("клиент {name} не получил кадр"))
            .expect("поток закрылся")
            .expect("ошибка чтения");
        let Message::Text(text) = frame else {
            panic!("ожидался текстовый кадр");
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["state"], "checking", "клиент {name}");
    }

    forge_bitrix::shutdown(&h.state).await;
}

// ---------------------------------------------------------------------------
// Приём файла ПО ПРОВОДУ (Ф9).
//
// Лестница `auth(409) → Content-Length(411) → limit(413) → filename(400)`
// проверяется юнит-тестами внутри плагина; здесь важно ДРУГОЕ — что отказ
// приходит, НЕ ДОЖИДАЯСЬ ТЕЛА. Через `oneshot` это не увидеть вовсе: там тело
// уже собрано в памяти к моменту вызова. Поэтому запросы ниже отправляются
// вручную по TCP с заведомо ложным `Content-Length`: если бы сервер ждал тело
// целиком, тест упал бы по таймауту, а не по ассерту.
// ---------------------------------------------------------------------------

/// Подключённое состояние без единого сетевого вызова.
async fn authorize(state: &Arc<B24State>) {
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                name: Some("Тест".into()),
                last_name: None,
                admin: false,
            },
            portal: PORTAL.into(),
        })
        .await;
}

/// Отправляет сырой HTTP-запрос и читает ответ целиком.
async fn raw_request(addr: SocketAddr, head: &str, body: &[u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(head.as_bytes()).await.unwrap();
    if !body.is_empty() {
        sock.write_all(body).await.unwrap();
    }
    sock.flush().await.unwrap();

    let mut raw = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut raw)).await;
    assert!(
        read.is_ok(),
        "ответ не пришёл за 5 с — значит сервер ждал тело целиком"
    );
    String::from_utf8_lossy(&raw).into_owned()
}

/// Без подключения — `409` ДО чтения тела.
///
/// Заявлено 64 МиБ, реально уходит десять байт. Ответ обязан прийти сразу.
#[tokio::test]
async fn test_upload_rejects_before_reading_the_body() {
    let h = start_harness().await;
    let head = format!(
        "POST /api/bitrix/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: probe.txt\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        h.addr,
        64 * 1024 * 1024
    );
    let text = raw_request(h.addr, &head, b"0123456789").await;
    assert!(text.starts_with("HTTP/1.1 409"), "ожидался 409: {text}");
    assert!(text.contains("\"not_authorized\""), "{text}");

    forge_bitrix::shutdown(&h.state).await;
}

/// Стриминговая загрузка без `Content-Length` (`Transfer-Encoding: chunked`) —
/// `411`: без размера ни проверить лимит, ни показать проценты нечем.
#[tokio::test]
async fn test_upload_chunked_without_content_length_is_411() {
    let h = start_harness().await;
    authorize(&h.state).await;

    let head = format!(
        "POST /api/bitrix/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: probe.txt\r\n\
         Transfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\n\
         5\r\nhello\r\n0\r\n\r\n",
        h.addr
    );
    let text = raw_request(h.addr, &head, b"").await;
    assert!(text.starts_with("HTTP/1.1 411"), "ожидался 411: {text}");
    assert!(text.contains("\"length_required\""), "{text}");

    forge_bitrix::shutdown(&h.state).await;
}

/// `Content-Length` больше лимита ⇒ `413` ДО чтения тела.
///
/// Тело не отправляется ВООБЩЕ. Ждал бы его сервер — тест упал бы по таймауту;
/// именно так фронт узнаёт об отказе мгновенно, а не после заливки мегабайтов.
/// Плюс инвариант «до диска дело не дошло»: каталог staging'а не создан.
#[tokio::test]
async fn test_upload_over_limit_is_413_before_reading_the_body() {
    let h = start_harness().await;
    authorize(&h.state).await;

    let declared = h.state.config.max_upload_bytes + 1;
    let head = format!(
        "POST /api/bitrix/upload HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/octet-stream\r\n\
         X-Forge-File-Name: huge.iso\r\n\
         Content-Length: {declared}\r\n\
         Connection: close\r\n\r\n",
        h.addr
    );
    let text = raw_request(h.addr, &head, b"").await;
    assert!(text.starts_with("HTTP/1.1 413"), "ожидался 413: {text}");
    assert!(text.contains("\"too_large\""), "{text}");
    assert!(
        !h.state.config.cache_dir.join("uploads").exists(),
        "каталог staging'а не должен создаваться при отказе по размеру"
    );
    assert!(h.state.uploads.lock().await.is_empty());

    forge_bitrix::shutdown(&h.state).await;
}

/// Happy path по проводу: файл принят, `upload_id` выдан, `.part` лежит на
/// диске, имя пережило percent-encoding.
///
/// В портал здесь никто не ходит — приём файла локальная операция (сеть
/// начинается в `POST /messages`), а `base_url_override` стенда указывает на
/// заведомо закрытый порт.
#[tokio::test]
async fn test_upload_stages_a_unicode_name_over_the_wire() {
    let h = start_harness().await;
    authorize(&h.state).await;

    let payload = b"FORGE probe payload\n";
    let resp = reqwest::Client::new()
        .post(format!("{}/api/bitrix/upload", h.base()))
        .header("content-type", "application/octet-stream")
        // encodeURIComponent('отчёт.pdf')
        .header("x-forge-file-name", "%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf")
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["file_name"], "отчёт.pdf");
    assert_eq!(json["size"], payload.len());
    assert_eq!(json["mime"], "application/pdf");
    let upload_id = json["upload_id"].as_str().unwrap();
    assert!(!upload_id.is_empty());

    let staged = h
        .state
        .config
        .cache_dir
        .join("uploads")
        .join(format!("{upload_id}.part"));
    assert!(staged.is_file(), "staging-файл не создан: {}", staged.display());
    assert_eq!(std::fs::read(&staged).unwrap(), payload);
    assert_eq!(h.state.uploads.lock().await.len(), 1);

    forge_bitrix::shutdown(&h.state).await;
}

/// СТРАЖ ИСКЛЮЧЕНИЯ CSRF: `POST /api/bitrix/upload` обязан стоять в
/// `RAW_UPLOAD_PATHS` хоста.
///
/// Слой `csrf_guard` требует `Content-Type: application/json` от каждого
/// мутирующего запроса — а тело загрузки это сырые байты. Выпади путь из
/// списка, и отправка файлов вернула бы `415` в remote-mode, продолжая
/// работать на localhost: дефект, который не воспроизводится у того, кто его
/// внёс.
///
/// Проверка идёт ПО ИСХОДНИКУ: `tmux-web` — бинарь без lib-таргета, и позвать
/// `auth::csrf_guard` из интеграционного теста нельзя. Поведение самого слоя
/// проверено юнит-тестами в `src/auth.rs` (`bitrix_upload_octet_stream_
/// same_origin_allowed` и соседние).
#[test]
fn test_upload_path_is_exempt_from_the_csrf_content_type_rule() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("tmux-web/src/auth.rs читается");
    let code: String = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let start = code
        .find("RAW_UPLOAD_PATHS")
        .expect("константа RAW_UPLOAD_PATHS не найдена");
    let end = code[start..].find("];").expect("список не закрыт") + start;
    let list = &code[start..end];
    assert!(
        list.contains("\"/api/bitrix/upload\""),
        "путь загрузки Битрикс24 выпал из RAW_UPLOAD_PATHS: {list}"
    );
    // И исключение обязано оставаться точечным: соседний мутирующий роут того
    // же плагина защиту по Content-Type терять не имеет права.
    assert!(
        !list.contains("/api/bitrix/dialogs"),
        "в списке исключений не должно быть ничего, кроме самой загрузки: {list}"
    );
}
