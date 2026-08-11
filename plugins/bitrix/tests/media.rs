//! Интеграционные тесты медиа-прокси (Ф8): путь скачивания, кэш, инвалидация,
//! безопасность ключа, дедуп, аватары.
//!
//! # Свой стенд, а не `b24_stub.rs`
//!
//! Стенд транспорта (`tests/b24_stub.rs`) знает ровно один маршрут — `POST
//! /rest/:uid/:code/:method`. Медиа же проверяется на ДВУХ разных маршрутах
//! сразу: REST-метод `im.v2.File.download` отдаёт адрес, а байты приезжают
//! обычным `GET` по этому адресу (для аватара — по публичному CDN портала).
//! Кроме того здесь нужен ЖУРНАЛ GET-запросов: именно им доказывается, что
//! `urlDownload`/`urlShow` не запрашиваются никогда.
//!
//! Порт всегда `127.0.0.1:0` — операционная система выдаёт свободный. Жёсткий
//! порт однажды упёрся бы в живой devforge на 7331 и сломал бы прогон на машине
//! разработчика.
//!
//! # Что здесь проверяется
//!
//! | тест | инвариант фазы |
//! |---|---|
//! | `the_only_download_path_is_im_v2_file_download` | `urlDownload`/`urlShow` не запрашиваются НИКОГДА |
//! | `hostile_media_keys_never_reach_the_file_system` | ключ парсится в числа, path traversal невозможен |
//! | `ten_parallel_requests_cost_one_portal_download` | дедуп inflight |
//! | `a_cache_hit_costs_no_portal_calls` | повторное открытие берёт из кэша |
//! | `an_edit_invalidates_the_cached_bytes` | `immutable` законен вместе с инвалидацией |
//! | `a_deleted_message_answers_404_without_touching_the_portal` | удалённое сообщение — 404 без сети |
//! | `a_large_file_is_streamed_in_chunks` | стрим, а не буферизация |
//! | `avatars_go_through_the_proxy` | Р6 |
//! | `a_user_without_an_avatar_is_a_404` | отсутствие аватара не ломает рендер |
//! | `the_media_response_never_redirects` | редирект вместо проксирования запрещён |

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::media_cache::{self, CacheKey};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow};

/// Код вебхука, который стенд обязан увидеть в пути REST-вызова.
const CODE: &str = "wqdzbfhmn83d23j4";

/// Пользователь вебхука.
const UID: &str = "1";

/// Диалог, чат-«полигон» (тот же, что на живом портале разведки).
const DIALOG: &str = "chat27";

/// Сообщение с вложением.
const MID: i64 = 113;

/// Файл на Диске портала.
const FILE_ID: i64 = 95;

/// Содержимое файла, которое отдаёт стенд.
const PAYLOAD: &[u8] = b"FORGE probe payload\n";

// ===========================================================================
// Стенд.
// ===========================================================================

/// Состояние стенда: журнал вызовов и сценарные переключатели.
#[derive(Default)]
struct StubState {
    /// Имена вызванных REST-методов по порядку.
    methods: Mutex<Vec<String>>,
    /// Пути ВСЕХ GET-запросов — ими доказывается, что чужие адреса не трогаем.
    gets: Mutex<Vec<String>>,
    /// Сколько раз стенд отдал байты файла.
    downloads: AtomicUsize,
    /// Что отдавать в качестве содержимого файла (правка меняет).
    payload: Mutex<Vec<u8>>,
    /// Есть ли у сообщения вложение.
    with_file: Mutex<bool>,
    /// Помечено ли сообщение удалённым.
    deleted: Mutex<bool>,
    /// Аватар пользователя: id → путь на CDN стенда (пусто ⇒ фото нет).
    avatars: Mutex<HashMap<i64, String>>,
    /// Искусственная задержка отдачи байтов — нужна тесту дедупа.
    delay_ms: Mutex<u64>,
    /// `host:port` слушателя: `downloadUrl` обязан быть АБСОЛЮТНЫМ, а адрес
    /// известен только после `bind`.
    host: Mutex<String>,
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

    fn gets(&self) -> Vec<String> {
        self.gets.lock().unwrap().clone()
    }
}

/// Поднятый стенд.
struct Stub {
    base_url: String,
    state: Arc<StubState>,
}

/// Поднимает стенд на свободном порту.
async fn start_stub() -> Stub {
    let state = Arc::new(StubState {
        payload: Mutex::new(PAYLOAD.to_vec()),
        with_file: Mutex::new(true),
        ..StubState::default()
    });
    let app = Router::new()
        .route("/rest/:uid/:code/:method", post(handle_method))
        // Адрес, который выдаёт `im.v2.File.download`. Он ЖИВЁТ ПОД ПУТЁМ С
        // КОДОМ ВЕБХУКА — ровно как на живом портале, поэтому тест-страж
        // утечки проверяет реальную форму, а не выдуманную.
        .route("/rest/:uid/:code/download/", get(handle_download))
        // Публичный CDN аватаров.
        .route("/cdn/:name", get(handle_cdn))
        // Ловушка: сюда ведут `urlDownload`/`urlShow` из дескриптора файла.
        // Единственная её задача — быть замеченной в журнале, если плагин
        // однажды решит ими воспользоваться.
        .route("/bitrix/services/main/ajax.php", get(handle_trap))
        .route("/file/:token", get(handle_trap))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // ДО спавна: обработчик читает адрес из состояния, и гонка «первый запрос
    // пришёл раньше, чем адрес записан» дала бы пустой downloadUrl.
    *state.host.lock().unwrap() = addr.to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Stub {
        base_url: format!("http://{addr}/rest/{UID}/{CODE}"),
        state,
    }
}

/// Блок `time` успешного ответа — той же формы, что у портала.
fn time_block() -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "start": now, "finish": now as f64 + 0.1, "duration": 0.1, "processing": 0,
        "date_start": "2026-08-05T20:14:58+03:00", "date_finish": "2026-08-05T20:14:59+03:00",
        "operating_reset_at": now + 600, "operating": 0.0
    })
}

fn ok(result: Value) -> Response {
    axum::Json(json!({"result": result, "time": time_block()})).into_response()
}

fn api_error(status: u16, code: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        axum::Json(json!({"error": code, "error_description": format!("stub: {code}")})),
    )
        .into_response()
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
        "profile" => ok(json!({"ID": "1", "ADMIN": true, "NAME": "Тест"})),
        "scope" => ok(json!(["im", "task"])),
        "im.dialog.messages.get" => ok(messages_page(&state)),
        "im.v2.File.download" => {
            let file_id = parsed.get("fileId").and_then(Value::as_i64).unwrap_or(0);
            if file_id != FILE_ID {
                return api_error(200, "FILE_NOT_FOUND");
            }
            // Адрес СОДЕРЖИТ КОД ВЕБХУКА — дословно как на живом портале.
            let host = state.host.lock().unwrap().clone();
            ok(json!({
                "downloadUrl":
                    format!("http://{host}/rest/{UID}/{CODE}/download/?token=im%7C{file_id}")
            }))
        }
        "im.user.list.get" => ok(user_map(&state)),
        "batch" => batch(&state, &parsed),
        _ => ok(json!(true)),
    }
}

/// Страница истории на одно сообщение.
fn messages_page(state: &Arc<StubState>) -> Value {
    let with_file = *state.with_file.lock().unwrap();
    let deleted = *state.deleted.lock().unwrap();
    let mut params = json!({});
    if with_file {
        params["FILE_ID"] = json!([FILE_ID]);
    }
    if deleted {
        params["IS_DELETED"] = json!("Y");
    }
    let files = if with_file {
        json!([{
            "id": FILE_ID, "chatId": 27, "name": "forge_probe.txt", "extension": "txt",
            "size": 500, "type": "file", "image": false, "status": "done", "authorId": 1,
            // Портал присылает эти поля ВСЕГДА. Плагин обязан их не замечать:
            // без сессионной куки они отдают HTML страницы логина.
            "urlDownload": "/bitrix/services/main/ajax.php?action=disk.api.file.download",
            "urlShow": "/bitrix/services/main/ajax.php?action=disk.api.file.show",
            "viewerAttrs": {"unifiedLink": "/file/iEi8j28ePgYmuLmBiNKj"},
            "mediaUrl": {"preview": {"250": ""}},
            "isVoiceNote": false, "isVideoNote": false
        }])
    } else {
        json!([])
    };
    json!({
        "chat_id": 27,
        "messages": [{
            "id": MID, "chat_id": 27, "author_id": 1,
            "date": "2026-08-05T20:20:36+03:00",
            "text": "вложение", "unread": false, "params": params
        }],
        "users": [],
        "files": files
    })
}

/// Ответ `im.user.list.get` — карта, как у портала.
fn user_map(state: &Arc<StubState>) -> Value {
    let avatars = state.avatars.lock().unwrap().clone();
    let mut map = serde_json::Map::new();
    for (id, url) in avatars {
        map.insert(
            id.to_string(),
            json!({"id": id, "name": format!("Сотрудник {id}"), "avatar": url,
                   "color": "#df532d", "status": "online"}),
        );
    }
    // Пользователь без фото: `avatar` приходит ЛОЖЬЮ, а не пустой строкой.
    map.insert(
        "42".into(),
        json!({"id": 42, "name": "Без фото", "avatar": false, "color": "#123456"}),
    );
    Value::Object(map)
}

/// `batch` — демультиплексируем `cmd` и отвечаем теми же ключами.
fn batch(state: &Arc<StubState>, parsed: &Value) -> Response {
    let mut result = serde_json::Map::new();
    if let Some(cmd) = parsed.get("cmd").and_then(Value::as_object) {
        for (key, value) in cmd {
            let command = value.as_str().unwrap_or_default();
            let method = command.split('?').next().unwrap_or_default();
            state.methods.lock().unwrap().push(method.to_string());
            let inner = match method {
                "im.user.list.get" => user_map(state),
                _ => json!(true),
            };
            result.insert(key.clone(), inner);
        }
    }
    ok(json!({
        "result": Value::Object(result),
        "result_error": [], "result_total": [], "result_next": [], "result_time": []
    }))
}

/// Отдача байтов по `downloadUrl`.
async fn handle_download(
    State(state): State<Arc<StubState>>,
    Path((_uid, code)): Path<(String, String)>,
    uri: axum::http::Uri,
) -> Response {
    assert_eq!(code, CODE);
    state
        .gets
        .lock()
        .unwrap()
        .push(uri.path_and_query().map(|p| p.to_string()).unwrap_or_default());
    state.downloads.fetch_add(1, Ordering::SeqCst);
    let delay = *state.delay_ms.lock().unwrap();
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    let body = state.payload.lock().unwrap().clone();
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/plain")
        .header(axum::http::header::CONTENT_LENGTH, body.len())
        .body(Body::from(body))
        .unwrap()
}

/// Публичный CDN аватаров.
async fn handle_cdn(State(state): State<Arc<StubState>>, Path(name): Path<String>) -> Response {
    state.gets.lock().unwrap().push(format!("/cdn/{name}"));
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "image/png")
        .body(Body::from(format!("PNG:{name}").into_bytes()))
        .unwrap()
}

/// Ловушка: сюда ведут неработающие адреса портала.
async fn handle_trap(State(state): State<Arc<StubState>>, uri: axum::http::Uri) -> Response {
    state
        .gets
        .lock()
        .unwrap()
        .push(uri.path_and_query().map(|p| p.to_string()).unwrap_or_default());
    // Живой портал отдаёт здесь HTML страницы логина при HTTP 200 — самая
    // коварная форма отказа: «успех», внутри которого нет файла.
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html")
        .body(Body::from("<html>login page</html>"))
        .unwrap()
}

// ===========================================================================
// Обвязка.
// ===========================================================================

/// Поднимает стенд, состояние и роутер плагина.
async fn harness() -> (tempfile::TempDir, Arc<StubState>, Arc<B24State>, Router) {
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
    let router = forge_bitrix::routes::build_router(state.clone());
    (dir, stub.state, state, router)
}

async fn send(app: &Router, uri: &str) -> Response {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_bytes(resp: Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

/// Путь прокси вложения (короткая форма — та, которой ходит фронт).
fn media_uri(dialog: &str, mid: i64, index: u32) -> String {
    format!("/api/bitrix/media/{dialog}/{mid}/{index}")
}

// ===========================================================================
// Тесты.
// ===========================================================================

/// ГЛАВНЫЙ ИНВАРИАНТ ФАЗЫ: байты приезжают ТОЛЬКО через
/// `im.v2.File.download` → `downloadUrl`, а `urlDownload`/`urlShow`/
/// `unifiedLink` не запрашиваются ни разу — они и не работают без сессионной
/// куки (живая проверка: HTML страницы логина при HTTP 200).
#[tokio::test]
async fn the_only_download_path_is_im_v2_file_download() {
    let (_d, stub, _st, app) = harness().await;

    let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("private, max-age=31536000, immutable"),
        "у вложения ответ immutable — он законен вместе с инвалидацией"
    );
    assert_eq!(body_bytes(resp).await, PAYLOAD);

    assert_eq!(stub.method_count("im.v2.File.download"), 1);
    assert_eq!(stub.downloads.load(Ordering::SeqCst), 1);
    let gets = stub.gets();
    assert_eq!(gets.len(), 1, "лишних GET быть не должно: {gets:?}");
    for path in &gets {
        assert!(
            !path.contains("ajax.php") && !path.contains("/file/"),
            "плагин пошёл по неработающему адресу портала: {path}"
        );
    }
}

/// Ключ кэша парсится в ЧИСЛА, поэтому подставной путь физически не может
/// добраться до файловой системы: файла вне каталога кэша не появляется и не
/// читается, а поход в портал не делается вовсе.
#[tokio::test]
async fn hostile_media_keys_never_reach_the_file_system() {
    let (dir, stub, _st, app) = harness().await;

    // Наборы РАЗНЫЕ по позициям: «0» — законный индекс вложения, но незаконный
    // диалог и незаконное сообщение.
    const ENTITY_JUNK: [&str; 8] = [
        "..%2F..%2Fetc",
        "%2e%2e",
        "chat27%00",
        "%D1%84%D0%B0%D0%B9%D0%BB", // «файл» кириллицей
        "0",
        "-1",
        "abc",
        "+113",
    ];
    const INDEX_JUNK: [&str; 6] = ["..%2F..", "%2e%2e", "abc", "-1", "20", "4294967296"];

    let mut uris: Vec<String> = Vec::new();
    for bad in ENTITY_JUNK {
        uris.push(format!("/api/bitrix/media/{bad}/113/0"));
        uris.push(format!("/api/bitrix/media/chat27/{bad}/0"));
        uris.push(format!("/api/bitrix/media/avatar/{bad}"));
    }
    for bad in INDEX_JUNK {
        uris.push(format!("/api/bitrix/media/chat27/113/{bad}"));
    }
    for uri in uris {
        let resp = send(&app, &uri).await;
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "{uri} обязан быть отвергнут разбором, получено {}",
            resp.status()
        );
    }

    assert_eq!(stub.methods(), Vec::<String>::new(), "портал не тревожили");
    // И на диске ничего не появилось — ни внутри кэша, ни рядом с ним.
    let root = dir.path();
    let mut found = Vec::new();
    collect(root, &mut found);
    assert!(
        found.iter().all(|p| p.starts_with(root)),
        "файл уехал за пределы каталога: {found:?}"
    );
    assert!(
        found.iter().all(|p| !p.to_string_lossy().contains("etc")),
        "подставной путь создал файл: {found:?}"
    );
}

/// Рекурсивный список файлов (для проверки, что кэш не вышел за каталог).
fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// ДЕДУП INFLIGHT: десять одновременных запросов одного вложения стоят ОДНОГО
/// похода в портал. Без него прокрутка ленты с галереей выела бы весь бюджет
/// 2 rps на повторные скачивания одного и того же файла.
#[tokio::test]
async fn ten_parallel_requests_cost_one_portal_download() {
    let (_d, stub, _st, app) = harness().await;
    // Задержка нужна, чтобы девять соседей ГАРАНТИРОВАННО застали владельца
    // ключа за работой, а не проскочили по готовому кэшу.
    *stub.delay_ms.lock().unwrap() = 120;

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
            let status = resp.status();
            (status, body_bytes(resp).await)
        }));
    }
    for t in tasks {
        let (status, body) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, PAYLOAD, "каждый запрос обязан получить целый файл");
    }

    assert_eq!(
        stub.downloads.load(Ordering::SeqCst),
        1,
        "десять запросов схлопнулись в одно скачивание"
    );
    assert_eq!(stub.method_count("im.v2.File.download"), 1);
    assert_eq!(
        stub.method_count("im.dialog.messages.get"),
        1,
        "и в один разбор страницы истории"
    );
}

/// Повторное открытие берёт файл из кэша: в портал не идём вовсе.
#[tokio::test]
async fn a_cache_hit_costs_no_portal_calls() {
    let (_d, stub, _st, app) = harness().await;

    assert_eq!(send(&app, &media_uri(DIALOG, MID, 0)).await.status(), StatusCode::OK);
    let after_first = stub.methods().len();

    for _ in 0..3 {
        let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, PAYLOAD);
    }
    assert_eq!(
        stub.methods().len(),
        after_first,
        "попадание в кэш не должно стоить ни одного вызова портала"
    );
    assert_eq!(stub.downloads.load(Ordering::SeqCst), 1);

    // Длинная форма пути ведёт в тот же кэш — это синонимы, а не два ресурса.
    let resp = send(&app, &format!("/api/bitrix/media/file/{DIALOG}/{MID}/0")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(stub.downloads.load(Ordering::SeqCst), 1);
}

/// `Cache-Control: immutable` законен ТОЛЬКО вместе с работающей
/// инвалидацией: после правки сообщения повторный запрос обязан отдать НОВЫЕ
/// байты, а не залипший кэш.
#[tokio::test]
async fn an_edit_invalidates_the_cached_bytes() {
    let (_d, stub, st, app) = harness().await;

    assert_eq!(body_bytes(send(&app, &media_uri(DIALOG, MID, 0)).await).await, PAYLOAD);

    // Собеседник заменил файл; портал отдаёт другое содержимое.
    *stub.payload.lock().unwrap() = b"REPLACED".to_vec();
    // Пока кэш не сброшен, клиент видит старое — и это правильно.
    assert_eq!(body_bytes(send(&app, &media_uri(DIALOG, MID, 0)).await).await, PAYLOAD);

    // Ровно это делают воркеры на правку (`recent_sync`) и на удаление
    // (`events`).
    let removed = media_cache::invalidate(&st.config, DIALOG, MID).await;
    assert!(removed >= 2, "снесены и байты, и мета: {removed}");

    let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, b"REPLACED");
    assert_eq!(stub.downloads.load(Ordering::SeqCst), 2);
}

/// Удалённое сообщение: портал байты больше не отдаёт, поэтому прокси обязан
/// отвечать `404` — и, узнав об удалении один раз, делать это БЕЗ СЕТИ.
#[tokio::test]
async fn a_deleted_message_answers_404_without_touching_the_portal() {
    let (_d, stub, st, app) = harness().await;
    *stub.deleted.lock().unwrap() = true;

    // Первый запрос ещё не знает об удалении: он читает страницу истории,
    // видит `IS_DELETED: "Y"` и запоминает это. Скачивания при этом не будет.
    let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        stub.method_count("im.v2.File.download"),
        0,
        "у удалённого сообщения адрес скачивания не запрашивается вовсе"
    );
    assert!(st.is_deleted(MID), "плагин запомнил удаление");

    // Все последующие — вообще без сети.
    let before = stub.methods().len();
    for _ in 0..3 {
        assert_eq!(
            send(&app, &media_uri(DIALOG, MID, 0)).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        stub.methods().len(),
        before,
        "404 удалённого сообщения обязан быть бесплатным"
    );

    // И кадр удаления из потока событий даёт тот же результат СРАЗУ, ещё до
    // первого запроса медиа.
    let (_d2, stub2, st2, app2) = harness().await;
    st2.note_deleted(MID);
    assert_eq!(
        send(&app2, &media_uri(DIALOG, MID, 0)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(stub2.methods(), Vec::<String>::new(), "ни одного вызова");
}

/// Сообщение без вложения и вложение за границей списка — `404 no_media`, а не
/// паника и не пустой файл в кэше.
#[tokio::test]
async fn a_message_without_the_requested_attachment_is_a_404() {
    let (_d, _stub, st, app) = harness().await;

    // Индекс 1 при единственном вложении.
    let resp = send(&app, &media_uri(DIALOG, MID, 1)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        !media_cache::contains(&st.config, &CacheKey::parse_file(DIALOG, "113", "1").unwrap()).await
    );

    // Сообщение без файлов вовсе.
    let (_d2, stub2, _st2, app2) = harness().await;
    *stub2.with_file.lock().unwrap() = false;
    assert_eq!(
        send(&app2, &media_uri(DIALOG, MID, 0)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(stub2.downloads.load(Ordering::SeqCst), 0);
}

/// Файл отдаётся ПОТОКОМ, а не одним куском: пиковая память прокси не зависит
/// от размера файла. Проверка косвенная — считаем кадры тела ответа.
#[tokio::test]
async fn a_large_file_is_streamed_in_chunks() {
    let (_d, stub, _st, app) = harness().await;
    // 4 МиБ при буфере отдачи 64 КиБ — заведомо больше одного кадра.
    let big = vec![7u8; 4 * 1024 * 1024];
    *stub.payload.lock().unwrap() = big.clone();

    let resp = send(&app, &media_uri(DIALOG, MID, 0)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok()),
        Some(big.len()),
        "длина объявляется заранее — она известна из меты, а не из чтения файла"
    );

    let mut stream = resp.into_body().into_data_stream();
    let mut frames = 0usize;
    let mut total = 0usize;
    let mut biggest = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        frames += 1;
        total += chunk.len();
        biggest = biggest.max(chunk.len());
    }
    assert_eq!(total, big.len());
    assert!(frames > 1, "тело приехало одним куском — это буферизация");
    assert!(
        biggest <= 64 * 1024,
        "кадр больше буфера чтения ({biggest} байт): стрим собирается в память"
    );
}

/// АВАТАРЫ (Р6): байты идут через прокси, хотя CDN портала публичен. Наружу
/// уходит только собственный путь плагина.
#[tokio::test]
async fn avatars_go_through_the_proxy() {
    let (_d, stub, st, app) = harness().await;
    let host = stub_host(&st);
    stub.avatars
        .lock()
        .unwrap()
        .insert(17, format!("http://{host}/cdn/face17.png"));

    let resp = send(&app, "/api/bitrix/media/avatar/17").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("private, max-age=3600"),
        "у аватара immutable НЕЗАКОНЕН: фото меняют без всяких событий"
    );
    assert_eq!(body_bytes(resp).await, b"PNG:face17.png");
    assert!(stub.gets().iter().any(|p| p == "/cdn/face17.png"));

    // Повторный запрос — из кэша, без обращения к CDN.
    let before = stub.gets().len();
    assert_eq!(send(&app, "/api/bitrix/media/avatar/17").await.status(), StatusCode::OK);
    assert_eq!(stub.gets().len(), before, "аватар взят из кэша");

    // Сменилось фото — сменился адрес. Событий об этом платформа не присылает
    // вовсе, поэтому единственный момент, когда плагин может о смене узнать, —
    // очередной ответ портала с блоком `user`. Ровно это и делает
    // `absorb_users`: и поток событий (`ONIMV2MESSAGEADD`), и
    // фолбэк-синхронизация зовут его на каждой пачке.
    let fresh: forge_bitrix::api::schema::UserRaw = serde_json::from_value(json!({
        "id": 17, "name": "Сотрудник 17", "color": "#df532d",
        "avatar": format!("http://{host}/cdn/face17-v2.png")
    }))
    .unwrap();
    st.peers.absorb_users(&[fresh]);

    let resp = send(&app, "/api/bitrix/media/avatar/17").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_bytes(resp).await,
        b"PNG:face17-v2.png",
        "отпечаток источника разошёлся — кэш обязан был обновиться сам"
    );

    // А пока адрес НЕ ИЗМЕНИЛСЯ, кэш переиспользуется: лишнего похода на CDN
    // проверка отпечатка не создаёт.
    let before = stub.gets().len();
    assert_eq!(
        send(&app, "/api/bitrix/media/avatar/17").await.status(),
        StatusCode::OK
    );
    assert_eq!(stub.gets().len(), before);
}

/// Хост стенда из конфига состояния.
fn stub_host(st: &Arc<B24State>) -> String {
    st.config
        .base_url_override
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_string()
}

/// У сотрудника нет фото — это `404` и штатная ветка «рисуй инициалы», а не
/// паника и не пустой файл в кэше.
#[tokio::test]
async fn a_user_without_an_avatar_is_a_404() {
    let (_d, stub, st, app) = harness().await;

    let resp = send(&app, "/api/bitrix/media/avatar/42").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        !media_cache::contains(&st.config, &CacheKey::parse_avatar("42").unwrap()).await,
        "пустой записи в кэше остаться не должно"
    );
    assert!(
        stub.gets().is_empty(),
        "за несуществующим фото никуда не ходим: {:?}",
        stub.gets()
    );

    // И повторный запрос не паникует.
    assert_eq!(
        send(&app, "/api/bitrix/media/avatar/42").await.status(),
        StatusCode::NOT_FOUND
    );
}

/// РЕДИРЕКТ ВМЕСТО ПРОКСИРОВАНИЯ ЗАПРЕЩЁН: заголовка `Location` не бывает ни в
/// одном ответе модуля, ни на успехе, ни на отказе. Иначе браузер получил бы
/// `downloadUrl`, то есть код вебхука целиком.
#[tokio::test]
async fn the_media_response_never_redirects() {
    let (_d, _stub, _st, app) = harness().await;

    for uri in [
        media_uri(DIALOG, MID, 0),
        media_uri(DIALOG, MID, 1),
        media_uri("chat999", 1, 0),
        "/api/bitrix/media/avatar/17".to_string(),
        "/api/bitrix/media/avatar/42".to_string(),
        "/api/bitrix/media/../../etc/0".to_string(),
    ] {
        let resp = send(&app, &uri).await;
        assert!(
            !resp.status().is_redirection(),
            "{uri} ответил редиректом {}",
            resp.status()
        );
        assert!(
            resp.headers().get(axum::http::header::LOCATION).is_none(),
            "{uri} отдал заголовок Location"
        );
    }
}

/// Без подключения портала медиа отвечает `409`, а не `404`: `404` означает
/// «плагина нет» и гасит кнопку вкладки. Проверяется и на ВРАЖДЕБНОМ ключе —
/// авторизация обязана срабатывать РАНЬШЕ разбора.
#[tokio::test]
async fn media_hits_auth_before_any_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        ..BitrixConfig::default()
    };
    let app = forge_bitrix::routes::build_router(B24State::new(cfg));

    for uri in [
        "/api/bitrix/media/chat27/113/0",
        "/api/bitrix/media/file/chat27/113/0",
        "/api/bitrix/media/%2e%2e/113/0",
        "/api/bitrix/media/chat27/113/999",
        "/api/bitrix/media/avatar/17",
        "/api/bitrix/media/avatar/abc",
    ] {
        let resp = send(&app, uri).await;
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "{uri} обязан отвечать 409 not_authorized"
        );
    }
}
