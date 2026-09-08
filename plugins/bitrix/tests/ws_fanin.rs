//! ТЕСТ-СТРАЖ АГРЕГАЦИИ WS: один сокет несёт кадры ВСЕХ порталов.
//!
//! # Что охраняется
//!
//! 1. **Портал, подключённый в рантайме, вещает в УЖЕ ОТКРЫТЫЙ сокет.** Без
//!    этого пользователь, добавивший второй портал, видел бы его ленту живой
//!    только после перезагрузки страницы — и списал бы это на «панель
//!    подвисла», а не на отсутствующую подписку.
//! 2. **Подписка появляется РАНЬШЕ снапшота** — и у портала, добавленного на
//!    живом процессе, тоже. Событие, случившееся между снапшотом и подпиской,
//!    восстановить неоткуда.
//! 3. **`Lagged` адресный.** Отстал один портал — `resync` уезжает с ЕГО
//!    `portal_id`, а кадры соседей продолжают идти. Общий сброс заставлял бы
//!    панель перечитывать по REST состояние всех порталов из-за одного.
//! 4. **Отвязанный портал перестаёт вещать**, а закрытие его канала НЕ рвёт
//!    соединение: остальные вкладки живы.
//! 5. **Каденс опроса считается ПО ПОРТАЛУ** (Ф5.3): один сокет даёт каждому
//!    порталу ровно одного подписчика — не N одному и не ноль остальным.
//!    Перебор каденса Bitrix наказывает не ошибкой, а разрывом TCP, поэтому
//!    ошибка здесь выглядела бы как «непонятные обрывы связи».
//!
//! # Герметичность
//!
//! Сокет не поднимается: проверяется [`forge_bitrix::ws::PortalFanIn`] — та
//! самая структура, которой владеет цикл соединения. Поднятый WebSocket
//! потребовал бы WS-клиента в dev-зависимостях, а проверял бы ровно то же
//! самое. Сети наружу нет: портал изображает стенд на `127.0.0.1:0`, пути
//! уведены в tempdir.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use forge_bitrix::config::{BitrixConfig, BitrixRootConfig, WebhookUrl};
use forge_bitrix::ws::{broadcast_frame, PortalFanIn, VACANT_PORTAL_ID};
use forge_bitrix::{B24Registry, B24ServerMsg};

/// Код вебхука первого портала.
const CODE_A: &str = "fanina0c0de123456";
/// Код второго — свой, чтобы реестр считал его ДРУГИМ порталом.
const CODE_B: &str = "faninb0c0de789012";

// ===========================================================================
// Стенд.
// ===========================================================================

/// Отвечает на всё, что нужно проверке вебхука: `profile` и `scope`.
async fn start_stub() -> String {
    async fn handle(
        State(()): State<()>,
        Path((_uid, _code, method)): Path<(String, String, String)>,
    ) -> Json<Value> {
        let result = match method.trim_end_matches(".json") {
            "profile" => json!({"ID": "1", "ADMIN": true, "NAME": "Тест"}),
            "scope" => json!(["im", "task"]),
            _ => json!(true),
        };
        Json(json!({
            "result": result,
            "time": {
                "start": 0.0, "finish": 0.0, "duration": 0.0, "processing": 0.0,
                "date_start": "", "date_finish": "", "operating": 0.0,
                "operating_reset_at": 0
            }
        }))
    }

    let app = Router::new()
        .route("/rest/:uid/:code/:method", post(handle))
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/rest/1/stubcode")
}

struct Harness {
    _dir: tempfile::TempDir,
    registry: Arc<B24Registry>,
}

/// Реестр из ОДНОГО пустого слота — ровно то, что видит свежая установка.
async fn fresh() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = start_stub().await;
    std::env::set_var("FORGE_BITRIX_CONFIG_PATH", dir.path().join("bitrix.toml"));

    let shared = BitrixConfig {
        cache_dir: dir.path().join("cache"),
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        base_url_override: base,
        connect_timeout_secs: 2,
        request_timeout_secs: 2,
        rate_limit_rps: 10,
        burst: 50,
        ..BitrixConfig::default()
    };
    let registry = B24Registry::new(BitrixRootConfig {
        shared,
        portals: Vec::new(),
        migrated_from_legacy: false,
    })
    .unwrap();

    Harness {
        _dir: dir,
        registry,
    }
}

/// Вебхук портала `id`.
fn webhook_of(id: &str, code: &str) -> WebhookUrl {
    WebhookUrl::new(format!("https://{id}.bitrix24.ru/rest/1/{code}/"))
}

/// Ждёт кадр от набора подписок, не давая тесту зависнуть навсегда.
async fn next_frame(fan: &mut PortalFanIn) -> (String, Option<B24ServerMsg>) {
    let (_, portal_id, ev) = tokio::time::timeout(std::time::Duration::from_secs(2), fan.recv())
        .await
        .expect("кадр не пришёл за две секунды");
    (portal_id, broadcast_frame(ev))
}

// ===========================================================================
// Проверки.
// ===========================================================================

/// Свежая установка: подписка есть даже на слот БЕЗ портала, и его кадры
/// подписаны зарезервированным `_vacant`, а не пустой строкой.
#[tokio::test]
async fn the_vacant_slot_is_subscribed_and_never_carries_an_empty_portal_id() {
    let h = fresh().await;
    let mut fan = PortalFanIn::new(&h.registry);
    assert_eq!(fan.len(), 1, "пустой слот тоже обязан быть подписан");

    h.registry.primary().broadcast_event(B24ServerMsg::Ping);
    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, VACANT_PORTAL_ID);
    assert_eq!(msg, Some(B24ServerMsg::Ping));
}

/// ГЛАВНАЯ проверка Ф5.1: портал, добавленный ПОСЛЕ открытия сокета, вещает
/// в него без переподключения фронта.
#[tokio::test]
async fn a_portal_added_at_runtime_reaches_an_already_open_socket() {
    let h = fresh().await;

    // «Сокет открыт»: подписка на состав и на broadcast всех слотов.
    let composition = h.registry.subscribe_composition();
    let mut fan = PortalFanIn::new(&h.registry);

    // Первый портал занимает пустой слот, второй встаёт рядом.
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    let beta = h
        .registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    // Сокет разбужен сигналом состава — именно так он узнаёт о новичке.
    assert!(
        composition.has_changed().unwrap(),
        "изменение состава обязано будить сокет"
    );
    let fresh_states = fan.sync(&h.registry);
    // ОБА портала — новые подписки. Занятие пустого слота с Ф6 пересобирает
    // состояние (дисковые пути выведены из идентификатора портала и лежат в
    // неизменяемом `BitrixConfig`), поэтому `Arc` там тоже новый. Ровно на этот
    // случай `sync` и сверяет подписки по `Arc::ptr_eq`, а не по `portal_id`:
    // сверка по имени оставила бы сокет на мёртвом слоте, и первый портал
    // молчал бы в уже открытой панели до переподключения.
    assert_eq!(
        fresh_states.len(),
        2,
        "и занятый слот, и добавленный рядом — новые подписки: {:?}",
        fresh_states.iter().map(|s| s.portal_id()).collect::<Vec<_>>()
    );
    assert_eq!(fan.len(), 2);

    // Событие НОВОГО портала доезжает, подписанное его именем.
    beta.broadcast_event(B24ServerMsg::Resync);
    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, "beta");
    assert_eq!(msg, Some(B24ServerMsg::Resync));

    // И первый продолжает вещать — под своим именем, а не под именем слота.
    h.registry
        .get("alpha")
        .unwrap()
        .broadcast_event(B24ServerMsg::Ping);
    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, "alpha");
    assert_eq!(msg, Some(B24ServerMsg::Ping));

    h.registry.shutdown_all().await;
}

/// Подписка на новичка появляется РАНЬШЕ, чем его снапшот: событие,
/// случившееся сразу после `sync`, не теряется.
#[tokio::test]
async fn the_subscription_precedes_the_snapshot_for_a_runtime_portal() {
    let h = fresh().await;
    let mut fan = PortalFanIn::new(&h.registry);
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    let beta = h
        .registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    let fresh_states = fan.sync(&h.registry);
    // Событие ровно в тот момент, когда цикл сокета собирал бы снапшот.
    beta.broadcast_event(B24ServerMsg::Ping);
    for state in fresh_states {
        let _ = state.login_state_dto().await;
    }

    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, "beta");
    assert_eq!(
        msg,
        Some(B24ServerMsg::Ping),
        "событие между подпиской и снапшотом обязано дойти"
    );

    h.registry.shutdown_all().await;
}

/// Р19 при агрегации: отставание ОДНОГО портала даёт `resync` только ему.
#[tokio::test]
async fn a_lagging_portal_resyncs_only_itself() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    h.registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    let mut fan = PortalFanIn::new(&h.registry);
    let alpha = h.registry.get("alpha").unwrap();
    let beta = h.registry.get("beta").unwrap();

    // Переполняем канал ТОЛЬКО у beta — так выглядит вкладка, которую усыпил
    // браузер, при живом соседнем портале.
    for _ in 0..(forge_bitrix::state::BROADCAST_CAPACITY + 8) {
        beta.broadcast_event(B24ServerMsg::Ping);
    }

    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, "beta", "resync обязан быть адресным");
    assert_eq!(msg, Some(B24ServerMsg::Resync));

    // Кадры alpha не пострадали: ни разрыва, ни лишнего resync'а.
    alpha.broadcast_event(B24ServerMsg::Resync);
    let mut saw_alpha = false;
    for _ in 0..(forge_bitrix::state::BROADCAST_CAPACITY + 16) {
        let (portal_id, msg) = next_frame(&mut fan).await;
        assert!(msg.is_some(), "соединение не должно закрываться");
        if portal_id == "alpha" {
            assert_eq!(msg, Some(B24ServerMsg::Resync));
            saw_alpha = true;
            break;
        }
    }
    assert!(saw_alpha, "кадры соседнего портала обязаны продолжаться");

    h.registry.shutdown_all().await;
}

/// Отвязка портала снимает его подписку, но соединение живо: остальные
/// вкладки продолжают получать кадры.
#[tokio::test]
async fn removing_a_portal_does_not_close_the_socket() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    h.registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    let composition = h.registry.subscribe_composition();
    let mut fan = PortalFanIn::new(&h.registry);
    assert_eq!(fan.len(), 2);

    assert!(h.registry.remove("beta", false).await.unwrap());
    assert!(
        composition.has_changed().unwrap(),
        "отвязка обязана будить сокет"
    );
    let fresh_states = fan.sync(&h.registry);
    assert_eq!(fan.len(), 1, "подписка на отвязанный портал снята");
    assert!(
        fresh_states.is_empty(),
        "новых слотов не появилось: alpha остался единственным"
    );

    h.registry
        .get("alpha")
        .unwrap()
        .broadcast_event(B24ServerMsg::Ping);
    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, "alpha");
    assert_eq!(msg, Some(B24ServerMsg::Ping));

    h.registry.shutdown_all().await;
}

/// Отвязка ПОСЛЕДНЕГО портала возвращает пустой слот — и на него появляется
/// подписка, иначе экран подключения не ожил бы без перезагрузки страницы.
#[tokio::test]
async fn the_restored_vacant_slot_is_subscribed_again() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();

    let mut fan = PortalFanIn::new(&h.registry);
    assert!(h.registry.remove("alpha", false).await.unwrap());

    let fresh_states = fan.sync(&h.registry);
    assert_eq!(fresh_states.len(), 1, "вернувшийся слот — новая подписка");
    assert_eq!(fan.len(), 1);

    h.registry.primary().broadcast_event(B24ServerMsg::Ping);
    let (portal_id, msg) = next_frame(&mut fan).await;
    assert_eq!(portal_id, VACANT_PORTAL_ID);
    assert_eq!(msg, Some(B24ServerMsg::Ping));

    h.registry.shutdown_all().await;
}

/// Переименование и перестановка тоже двигают состав: строка вкладок
/// перерисовывается кадром `portals`, а не по перезагрузке страницы.
#[tokio::test]
async fn renaming_and_reordering_wake_the_socket_too() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    h.registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    let mut composition = h.registry.subscribe_composition();
    assert!(!composition.has_changed().unwrap(), "исходно тишина");

    assert!(h.registry.rename("beta", "Бета").await.unwrap());
    assert!(composition.has_changed().unwrap(), "rename будит сокет");
    let _ = composition.borrow_and_update();

    h.registry
        .reorder(&["beta".to_string(), "alpha".to_string()])
        .await
        .unwrap();
    assert!(composition.has_changed().unwrap(), "reorder будит сокет");

    h.registry.shutdown_all().await;
}

/// Ф5.3: ОДИН сокет даёт КАЖДОМУ порталу ровно одного подписчика — и ни
/// одному не даёт N.
#[tokio::test]
async fn one_socket_gives_every_portal_exactly_one_subscriber() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();
    h.registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();
    let alpha = h.registry.get("alpha").unwrap();
    let beta = h.registry.get("beta").unwrap();

    assert_eq!(alpha.ws_subscribers(), 0, "панель ещё не открыта");
    assert_eq!(beta.ws_subscribers(), 0);

    let fan = PortalFanIn::new(&h.registry);
    assert_eq!(alpha.ws_subscribers(), 1, "ровно один, а не два");
    assert_eq!(beta.ws_subscribers(), 1, "и соседний портал тоже активен");

    // Второй сокет — второй подписчик у каждого, но каденс от этого не
    // меняется: он смотрит на «больше нуля», а не на само число.
    let second = PortalFanIn::new(&h.registry);
    assert_eq!(alpha.ws_subscribers(), 2);
    assert_eq!(beta.ws_subscribers(), 2);
    drop(second);
    assert_eq!(alpha.ws_subscribers(), 1);
    assert_eq!(beta.ws_subscribers(), 1);

    // Закрытие сокета возвращает idle-каденс ВСЕМ порталам сразу.
    drop(fan);
    assert_eq!(alpha.ws_subscribers(), 0);
    assert_eq!(beta.ws_subscribers(), 0);

    h.registry.shutdown_all().await;
}

/// Ф5.3: портал, добавленный при открытом сокете, получает active-каденс
/// сразу — не дожидаясь переподключения панели.
#[tokio::test]
async fn a_runtime_portal_gets_the_active_cadence_at_once() {
    let h = fresh().await;
    h.registry
        .add(&webhook_of("alpha", CODE_A), None)
        .await
        .unwrap();

    let mut fan = PortalFanIn::new(&h.registry);
    let beta = h
        .registry
        .add(&webhook_of("beta", CODE_B), None)
        .await
        .unwrap();

    // До пересборки набора подписчиков у новичка нет — и каденс у него
    // idle'ный, что честно: сокет о нём ещё не знает.
    let timings = &beta.poll;
    assert_eq!(
        forge_bitrix::events::next_interval(timings, false, beta.ws_subscribers()),
        timings.idle
    );

    fan.sync(&h.registry);
    assert_eq!(beta.ws_subscribers(), 1);
    assert_eq!(
        forge_bitrix::events::next_interval(timings, false, beta.ws_subscribers()),
        timings.active,
        "подключённый при живой панели портал обязан опрашиваться часто"
    );

    // Закрытие панели возвращает редкий каденс и ему.
    drop(fan);
    assert_eq!(
        forge_bitrix::events::next_interval(timings, false, beta.ws_subscribers()),
        timings.idle
    );

    h.registry.shutdown_all().await;
}
