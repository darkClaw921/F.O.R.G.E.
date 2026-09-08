//! ТЕСТ-СТРАЖ РОУТОВ УПРАВЛЕНИЯ ПОРТАЛАМИ (`/api/bitrix/portals`).
//!
//! # Что охраняется
//!
//! 1. **Секрет не выходит наружу.** Ни в списке вкладок, ни в ответе на
//!    добавление не должно быть ни кода вебхука, ни сегмента `/rest/`, ни
//!    хоста портала. Тип `WebhookUrl` не сериализуется, но DTO собирается
//!    вручную — и запретить руками положить туда `expose()` может только тест.
//! 2. **Вкладки-призрака не бывает.** Реестр НИКОГДА не пуст: на свежей
//!    установке в нём стоит слот с пустым вебхуком. Отдай его в список — и
//!    фронт нарисует безымянную, неадресуемую и неудаляемую вкладку.
//! 3. **Неудачный ввод не оставляет следов** — ни портала в списке, ни секрета
//!    на диске.
//! 4. **Один вебхук — один портал.** Второе состояние на тот же секрет
//!    означало бы второй троттлер на общий бюджет 2 rps, а портал на
//!    превышение отвечает не ошибкой, а разрывом TCP.
//! 5. **Отвязка адресна.** `DELETE /portals/:pid` и `POST /logout` снимают
//!    ОДИН портал, а не весь список.
//!
//! # Герметичность
//!
//! Сети наружу нет: портал изображает стенд на `127.0.0.1:0` (порт выдаёт
//! ОС), а плагин смотрит на него через `base_url_override`. Ни `~/.forge`, ни
//! `~/.config/forge` не трогаются — все пути уводятся в tempdir.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, BitrixRootConfig};
use forge_bitrix::B24Registry;

/// Код вебхука. Не имеет права появиться ни в одном ответе.
const CODE: &str = "portalsc0de123456";

/// Код второго портала — свой, чтобы `add` считал его ДРУГИМ порталом.
const CODE2: &str = "portalsc0de789012";

// ===========================================================================
// Стенд портала.
// ===========================================================================

/// Стенд отвечает на всё, что нужно проверке вебхука: `profile` и `scope`.
///
/// Один стенд на оба портала намеренно: различать их обязан реестр, а не
/// случайное расхождение адресов.
async fn start_stub() -> String {
    // Все три сегмента пути обязаны быть в кортеже: `Path<String>` на маршруте
    // с тремя параметрами не извлекается вовсе, и стенд молча отвечал бы 400.
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
    // База включает `/rest/<uid>/<code>` целиком: клиент дописывает к ней
    // только `<method>.json` (см. `B24Client::new`).
    format!("http://{addr}/rest/1/stubcode")
}

// ===========================================================================
// Стенд плагина.
// ===========================================================================

struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    registry: Arc<B24Registry>,
    config_path: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
}

impl Harness {
    /// Содержимое конфига (пустая строка, если файла ещё нет).
    fn config_text(&self) -> String {
        std::fs::read_to_string(&self.config_path).unwrap_or_default()
    }
}

/// Реестр из ОДНОГО пустого слота — ровно то, что видит свежая установка.
///
/// `FORGE_BITRIX_CONFIG_PATH` выставляется намеренно: без него правило Р17
/// отвергает `http://127.0.0.1` в `base_url_override`, и стенд просто не
/// поднялся бы. Заодно это тот самый признак изолированного окружения, ради
/// которого переменная и существует. Значение — путь внутри tempdir, но
/// реестр читает и пишет `shared.config_path`, который задан явно.
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
        // Реестр валидирует общие настройки (в отличие от `B24State::new`),
        // поэтому лимит — в разрешённых пределах. Тесту он не мешает: пачка
        // запросов здесь меньше `burst`.
        rate_limit_rps: 10,
        burst: 50,
        ..BitrixConfig::default()
    };
    let cache_dir = shared.cache_dir.clone();
    let config_path = shared.config_path.clone();

    let registry = B24Registry::new(BitrixRootConfig {
        shared,
        portals: Vec::new(),
        migrated_from_legacy: false,
    })
    .unwrap();
    let app = forge_bitrix::routes::build_router(registry.clone());

    Harness {
        _dir: dir,
        app,
        registry,
        config_path,
        cache_dir,
    }
}

/// Адрес вебхука портала `id` на стенде.
fn webhook_of(id: &str, code: &str) -> String {
    format!("https://{id}.bitrix24.ru/rest/1/{code}/")
}

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

/// Добавляет портал и возвращает тело ответа.
async fn add(h: &Harness, id: &str, code: &str, title: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({"webhook": webhook_of(id, code)});
    if let Some(t) = title {
        body["title"] = json!(t);
    }
    request(&h.app, "POST", "/api/bitrix/portals", None, Some(body)).await
}

async fn list(h: &Harness) -> Value {
    let (status, v) = request(&h.app, "GET", "/api/bitrix/portals", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

/// Идентификаторы вкладок в порядке ответа.
fn ids(v: &Value) -> Vec<String> {
    v["portals"]
        .as_array()
        .expect("portals — массив")
        .iter()
        .map(|p| p["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn text(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}

// ===========================================================================
// Ф4.1 — список.
// ===========================================================================

/// Свежая установка ⇒ ПУСТОЙ список, хотя слот в реестре есть.
///
/// Слот без вебхука — это место для портала, а не портал: отдай его сюда, и
/// фронт нарисует вкладку-призрак.
#[tokio::test]
async fn a_fresh_install_shows_no_tabs_although_the_registry_is_not_empty() {
    let h = fresh().await;
    assert_eq!(h.registry.len(), 1, "реестр не бывает пустым");

    let v = list(&h).await;
    assert_eq!(ids(&v), Vec::<String>::new(), "{v}");
}

/// ИНВАРИАНТ ФАЗЫ: в списке нет ни кода вебхука, ни `/rest/`, ни хоста.
#[tokio::test]
async fn the_portal_list_never_leaks_the_webhook() {
    let h = fresh().await;
    let (status, added) = add(&h, "alpha", CODE, None).await;
    assert_eq!(status, StatusCode::OK, "{added}");

    for (what, v) in [("ответ POST", added), ("список", list(&h).await)] {
        let body = text(&v);
        assert!(!body.contains(CODE), "код вебхука в {what}: {body}");
        assert!(!body.contains("/rest/"), "сегмент /rest/ в {what}: {body}");
        assert!(
            !body.contains("bitrix24.ru"),
            "хост портала в {what}: {body}"
        );
        assert!(
            !body.contains("fingerprint"),
            "отпечаток вебхука в {what}: {body}"
        );
    }
}

/// Порядок вкладок в ответе = порядок реестра.
#[tokio::test]
async fn the_list_order_is_the_registry_order() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    assert_eq!(ids(&list(&h).await), vec!["alpha", "beta"]);
    assert_eq!(h.registry.ids(), vec!["alpha", "beta"]);
}

/// Вкладка несёт состояние подключения — той же формы, что `/status`.
#[tokio::test]
async fn a_tab_carries_the_login_state_of_its_own_portal() {
    let h = fresh().await;
    add(&h, "alpha", CODE, Some("Рабочий")).await;

    let v = list(&h).await;
    assert_eq!(v["portals"][0]["id"], "alpha");
    assert_eq!(v["portals"][0]["title"], "Рабочий");
    assert_eq!(v["portals"][0]["login"]["state"], "authorized");
    assert_eq!(v["portals"][0]["unread"], 0, "снимка счётчиков ещё нет");
}

// ===========================================================================
// Ф4.2 — добавление.
// ===========================================================================

/// Успешное добавление даёт портал в реестре И запись в конфиге.
#[tokio::test]
async fn a_successful_add_lands_both_in_the_registry_and_in_the_config() {
    let h = fresh().await;
    let (status, v) = add(&h, "alpha", CODE, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["id"], "alpha");

    assert_eq!(h.registry.ids(), vec!["alpha"]);
    assert!(
        h.config_text().contains(CODE),
        "секрет не записан: {}",
        h.config_text()
    );
}

/// Мусорный ввод НЕ создаёт портала и НЕ пишет секрет.
///
/// Разбор — та же [`forge_bitrix::login::parse_webhook_input`], что на экране
/// подключения: второго парсера у плагина нет и быть не должно.
#[tokio::test]
async fn a_rejected_webhook_creates_no_portal_and_writes_nothing() {
    let h = fresh().await;

    for garbage in [
        "",
        "   ",
        "не адрес вовсе",
        "https://p.bitrix24.ru/",
        "http://evil.example/rest/1/code/",
        "https://p.bitrix24.ru/rest//code/",
    ] {
        let (status, v) = request(
            &h.app,
            "POST",
            "/api/bitrix/portals",
            None,
            Some(json!({"webhook": garbage})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{garbage:?} → {v}");
        assert_eq!(v["error"]["code"], "invalid_input", "{garbage:?} → {v}");
    }

    assert_eq!(ids(&list(&h).await), Vec::<String>::new());
    assert_eq!(h.registry.len(), 1, "пустой слот на месте");
    assert!(
        !h.config_path.exists(),
        "мусорный ввод не должен создавать конфиг"
    );
}

/// Повторное добавление ТОГО ЖЕ вебхука не плодит дубликат — оно обновляет
/// имя.
///
/// Два состояния на один секрет означали бы два троттлера на общий бюджет
/// 2 rps, а портал на превышение отвечает разрывом TCP, а не ошибкой.
#[tokio::test]
async fn adding_the_same_webhook_twice_only_updates_the_title() {
    let h = fresh().await;
    add(&h, "alpha", CODE, Some("Первое имя")).await;
    let (status, v) = add(&h, "alpha", CODE, Some("Второе имя")).await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let v = list(&h).await;
    assert_eq!(ids(&v), vec!["alpha"], "дубликат вкладки");
    assert_eq!(v["portals"][0]["title"], "Второе имя");
}

/// РАЗНЫЕ порталы с совпадающим первым сегментом хоста получают РАЗНЫЕ `id`.
#[tokio::test]
async fn a_second_portal_with_a_colliding_id_gets_a_suffix() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;

    let (status, v) = request(
        &h.app,
        "POST",
        "/api/bitrix/portals",
        None,
        Some(json!({"webhook": format!("https://alpha.example.com/rest/1/{CODE2}/")})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["id"], "alpha-2", "коллизия id разрешается суффиксом");
    assert_eq!(ids(&list(&h).await), vec!["alpha", "alpha-2"]);
}

// ===========================================================================
// Ф4.3 — переименование, порядок, отвязка.
// ===========================================================================

#[tokio::test]
async fn renaming_a_portal_changes_only_its_title() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    let (status, v) = request(
        &h.app,
        "PATCH",
        "/api/bitrix/portals/alpha",
        None,
        Some(json!({"title": "  Рабочий  "})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["portals"][0]["title"], "Рабочий", "имя не обрезано");
    assert_eq!(v["portals"][1]["title"], "", "сосед переименован");
    assert_eq!(ids(&v), vec!["alpha", "beta"], "порядок не тронут");

    // Пустая строка возвращает автоматическую подпись.
    let (_, v) = request(
        &h.app,
        "PATCH",
        "/api/bitrix/portals/alpha",
        None,
        Some(json!({"title": ""})),
    )
    .await;
    assert_eq!(v["portals"][0]["title"], "");
}

#[tokio::test]
async fn reordering_moves_the_tabs_in_both_the_registry_and_the_answer() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    let (status, v) = request(
        &h.app,
        "PATCH",
        "/api/bitrix/portals/alpha",
        None,
        Some(json!({"order": ["beta", "alpha"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), vec!["beta", "alpha"]);
    assert_eq!(h.registry.ids(), vec!["beta", "alpha"]);
}

/// Пустое тело PATCH — не ошибка: «ничего не менять».
#[tokio::test]
async fn an_empty_patch_body_is_not_an_error() {
    let h = fresh().await;
    add(&h, "alpha", CODE, Some("Рабочий")).await;

    for body in [None, Some(json!({}))] {
        let (status, v) = request(&h.app, "PATCH", "/api/bitrix/portals/alpha", None, body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["portals"][0]["title"], "Рабочий");
    }
}

/// Неизвестный `:pid` — 404 в общем конверте, и НИ ОДНОГО следа чужого ввода
/// в ответе.
#[tokio::test]
async fn an_unknown_pid_is_a_404_for_both_patch_and_delete() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;

    for (method, body) in [
        ("PATCH", Some(json!({"title": "чужое"}))),
        ("DELETE", None),
    ] {
        for pid in ["gamma", "%2e%2e", "alpha-2"] {
            let (status, v) = request(
                &h.app,
                method,
                &format!("/api/bitrix/portals/{pid}"),
                None,
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {pid} → {v}");
            assert_eq!(v["error"]["code"], "unknown_portal", "{method} {pid} → {v}");
        }
    }

    assert_eq!(ids(&list(&h).await), vec!["alpha"], "список не тронут");
}

/// Отвязка снимает ОДИН портал, стирает его секрет и оставляет соседа целым.
#[tokio::test]
async fn delete_unlinks_exactly_one_portal_and_wipes_its_secret() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    let (status, v) = request(&h.app, "DELETE", "/api/bitrix/portals/alpha", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), vec!["beta"]);

    let cfg = h.config_text();
    assert!(!cfg.contains(CODE), "секрет отвязанного портала цел: {cfg}");
    assert!(cfg.contains(CODE2), "секрет соседа стёрт: {cfg}");
}

/// После отвязки запрос с заголовком удалённого портала — 404, а не 500 и не
/// молчаливый сполз на соседа.
#[tokio::test]
async fn after_a_delete_the_old_portal_header_is_a_404() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;
    request(&h.app, "DELETE", "/api/bitrix/portals/alpha", None, None).await;

    let (status, v) = request(&h.app, "GET", "/api/bitrix/status", Some("alpha"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "unknown_portal");

    // Повторный DELETE — тоже 404: удалять уже нечего.
    let (status, _) = request(&h.app, "DELETE", "/api/bitrix/portals/alpha", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Отвязка ПОСЛЕДНЕГО портала оставляет процесс живым и возвращает фронт на
/// экран подключения.
#[tokio::test]
async fn deleting_the_last_portal_returns_the_connection_screen() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;

    let (status, v) = request(&h.app, "DELETE", "/api/bitrix/portals/alpha", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), Vec::<String>::new());
    assert_eq!(h.registry.len(), 1, "на место портала встаёт пустой слот");

    // `/status` без заголовка отвечает 200 и показывает экран подключения.
    let (status, v) = request(&h.app, "GET", "/api/bitrix/status", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["login"]["state"], "not_configured");
    assert!(!h.config_text().contains(CODE), "{}", h.config_text());
}

/// `?purge_cache=1` до Ф6 НЕ сносит общий каталог кэша.
///
/// Пер-портальных подкаталогов ещё нет, все порталы делят один каталог, и
/// снести его значило бы стереть кэш соседей. Тест фиксирует именно эту
/// границу: флаг принимается, ответ 200, чужие байты целы.
#[tokio::test]
async fn purge_cache_refuses_to_wipe_the_shared_cache_directory() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    std::fs::create_dir_all(&h.cache_dir).unwrap();
    let neighbour = h.cache_dir.join("neighbour.bin");
    std::fs::write(&neighbour, b"cache").unwrap();

    let (status, v) = request(
        &h.app,
        "DELETE",
        "/api/bitrix/portals/alpha?purge_cache=1",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(neighbour.exists(), "снесён ОБЩИЙ кэш соседей");
}

// ===========================================================================
// Ф4.4 — старые роуты экрана подключения.
// ===========================================================================

/// `POST /api/bitrix/login/webhook` работает с АДРЕСУЕМЫМ порталом.
#[tokio::test]
async fn the_old_login_route_targets_the_addressed_portal() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    // Пустой слот кончился — старый экран входа адресуется заголовком.
    let (status, v) = request(
        &h.app,
        "POST",
        "/api/bitrix/login/webhook",
        Some("beta"),
        Some(json!({"webhook": webhook_of("beta", CODE2)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "authorized");
    assert_eq!(ids(&list(&h).await), vec!["alpha", "beta"], "состав цел");
}

/// Перевыпущенный вебхук (тот же хост, другой код) МЕНЯЕТ секрет вкладки, а
/// не заводит вторую.
///
/// Это единственная причина, по которой экран подключения открывают у уже
/// подключённого портала: вебхук отозвали и выпустили заново.
#[tokio::test]
async fn a_reissued_webhook_replaces_the_secret_of_the_same_tab() {
    let h = fresh().await;
    add(&h, "alpha", CODE, Some("Рабочий")).await;
    add(&h, "beta", CODE2, None).await;

    const REISSUED: &str = "reissuedc0de99999";
    let (status, v) = request(
        &h.app,
        "POST",
        "/api/bitrix/login/webhook",
        Some("alpha"),
        Some(json!({"webhook": webhook_of("alpha", REISSUED)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "authorized");

    let v = list(&h).await;
    assert_eq!(ids(&v), vec!["alpha", "beta"], "вкладка не раздвоилась");
    assert_eq!(v["portals"][0]["title"], "Рабочий", "имя вкладки потеряно");

    let cfg = h.config_text();
    assert!(cfg.contains(REISSUED), "новый секрет не записан: {cfg}");
    assert!(!cfg.contains(CODE), "старый секрет остался: {cfg}");
    assert!(cfg.contains(CODE2), "стёрт секрет соседа: {cfg}");
}

/// Вебхук ЧУЖОГО портала, вставленный в чужую вкладку, заводит НОВУЮ вкладку и
/// не трогает старую: «вставил не туда» не должно означать «отключил портал».
#[tokio::test]
async fn a_webhook_of_another_host_adds_a_tab_instead_of_replacing_one() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;

    let (status, v) = request(
        &h.app,
        "POST",
        "/api/bitrix/login/webhook",
        Some("alpha"),
        Some(json!({"webhook": webhook_of("beta", CODE2)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");

    assert_eq!(ids(&list(&h).await), vec!["alpha", "beta"]);
    let cfg = h.config_text();
    assert!(cfg.contains(CODE) && cfg.contains(CODE2), "{cfg}");
}

/// `POST /api/bitrix/logout` отвязывает ТОЛЬКО адресуемый портал.
///
/// Историческая семантика `clear_webhook_to` («снести обе формы записи»)
/// отвязывала бы ВСЕ порталы разом — для строки вкладок это заведомо не то,
/// чего ждёт пользователь, нажимая «выйти» на одной из них.
#[tokio::test]
async fn logout_unlinks_only_the_addressed_portal() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;

    let (status, v) = request(&h.app, "POST", "/api/bitrix/logout", Some("alpha"), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "not_configured");

    assert_eq!(ids(&list(&h).await), vec!["beta"], "сосед отвязан вместе");
    let cfg = h.config_text();
    assert!(!cfg.contains(CODE), "секрет не стёрт: {cfg}");
    assert!(cfg.contains(CODE2), "секрет соседа стёрт: {cfg}");
}

/// Повторный logout — не ошибка, но и не 500: портала уже нет, значит 404.
#[tokio::test]
async fn a_second_logout_of_the_same_portal_is_a_404() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;
    add(&h, "beta", CODE2, None).await;
    request(&h.app, "POST", "/api/bitrix/logout", Some("alpha"), None).await;

    let (status, v) = request(&h.app, "POST", "/api/bitrix/logout", Some("alpha"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "unknown_portal");
}

/// Logout ПОСЛЕДНЕГО портала возвращает экран подключения, как и раньше.
#[tokio::test]
async fn logging_out_of_the_last_portal_returns_the_connection_screen() {
    let h = fresh().await;
    add(&h, "alpha", CODE, None).await;

    let (status, v) = request(&h.app, "POST", "/api/bitrix/logout", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "not_configured");

    assert_eq!(ids(&list(&h).await), Vec::<String>::new());
    let (status, v) = request(&h.app, "GET", "/api/bitrix/status", None, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["login"]["state"], "not_configured");
    assert!(!h.config_text().contains(CODE), "{}", h.config_text());
}

/// Экран подключения свежей установки цел: пустой слот принимает вебхук
/// старым роутом и становится порталом.
#[tokio::test]
async fn the_connection_screen_of_a_fresh_install_still_works() {
    let h = fresh().await;

    let (status, v) = request(
        &h.app,
        "POST",
        "/api/bitrix/login/webhook",
        None,
        Some(json!({"webhook": webhook_of("alpha", CODE)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "authorized");
    assert_eq!(ids(&list(&h).await), vec!["alpha"], "вкладка появилась");
}
