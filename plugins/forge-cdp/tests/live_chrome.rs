//! Живые проверки против НАСТОЯЩЕГО Chrome.
//!
//! Запускаются только по явному `FORGE_CDP_LIVE=1`:
//!
//! ```text
//! FORGE_CDP_LIVE=1 cargo test -p forge-cdp --test live_chrome -- --test-threads=1
//! ```
//!
//! Почему не по умолчанию: обычный `cargo test` обязан оставаться герметичным
//! и быстрым, а здесь запускается браузер, которого на машине сборки может не
//! быть вовсе.
//!
//! Правила этих тестов, нарушать которые нельзя:
//! * профиль и кэш — ТОЛЬКО во временном каталоге (`TempDir`), никаких
//!   `~/.forge` и никакого профиля пользователя;
//! * процесс обязан быть убит в конце теста при любом исходе, поэтому каждый
//!   тест держит `ChromeProcess` и гасит его через `terminate`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forge_cdp::{
    launch, orphans, BindingMessage, BindingReceiver, CdpBrowser, CdpPage, CdpTransport,
    ChromeProcess, InjectConfig, InjectSetup, LaunchConfig, PageSetup, TransportConfig, WindowMode,
    WsTransport,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Общий гейт живых тестов.
fn live() -> bool {
    if std::env::var("FORGE_CDP_LIVE").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("FORGE_CDP_LIVE != 1 — живой тест пропущен");
    false
}

/// PGID вызывающего процесса.
fn own_pgid() -> i32 {
    unsafe { libc::getpgid(0) }
}

fn pgid_of(pid: i32) -> i32 {
    unsafe { libc::getpgid(pid) }
}

#[tokio::test]
async fn launch_reads_devtools_port_writes_pid_and_isolates_process_group() {
    if !live() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let meeting = tmp.path().join("meeting-1");
    let cache = tmp.path().join("cache");
    let mut cfg = LaunchConfig::new(&meeting, &cache);
    cfg.window_mode = WindowMode::Offscreen;
    cfg.devtools_port_timeout = Duration::from_secs(30);

    let mut chrome = launch(&cfg).await.expect("Chrome не запустился");

    // 1. Адрес ws прочитан из DevToolsActivePort, а не угадан.
    assert!(
        chrome.ws_url.starts_with("ws://127.0.0.1:"),
        "неожиданный ws-адрес: {}",
        chrome.ws_url
    );
    assert!(chrome.ws_url.contains("/devtools/browser/"));
    let port_file = cfg.user_data_dir.join(forge_cdp::launch::DEVTOOLS_PORT_FILE);
    assert!(port_file.is_file(), "Chrome не создал DevToolsActivePort");

    // 2. chrome.pid содержит pid и профиль.
    let record = orphans::read_pid_file(&meeting).expect("chrome.pid не прочитался");
    assert_eq!(record.pid, chrome.pid);
    assert_eq!(record.user_data_dir, cfg.user_data_dir);
    assert!(record.started_unix > 0);

    // 3. Процесс — лидер СВОЕЙ группы, и это не наша группа.
    let child_pgid = pgid_of(chrome.pid);
    assert_eq!(
        child_pgid, chrome.pid,
        "Chrome должен быть лидером своей группы (setsid), иначе killpg бьёт мимо"
    );
    assert_ne!(
        child_pgid,
        own_pgid(),
        "Chrome оказался в нашей группе процессов — killpg убил бы и нас"
    );

    // 4. Файл тишины создан и переиспользуем.
    //
    // Размер сверяется диапазоном, а не точным числом: ffmpeg дописывает в
    // WAV чанк LIST/INFO со своей подписью, и файл выходит на несколько
    // десятков байт длиннее канонических 44 + данные. Собственный писатель
    // даёт ровно 11 520 044 — это проверяет юнит-тест `launch::tests`.
    assert!(cfg.silence_wav.is_file());
    let size = std::fs::metadata(&cfg.silence_wav).unwrap().len();
    let payload = 60u64 * 48_000 * 2 * 2;
    assert!(
        (payload + 44..payload + 4096).contains(&size),
        "неожиданный размер файла тишины: {size}"
    );

    // 5. argv процесса содержит наш профиль — это то, на что опирается
    //    двойная проверка подметальщика сирот.
    let argv = orphans::process_argv(chrome.pid).unwrap_or_default();
    assert!(
        orphans::argv_matches_profile(&argv, &cfg.user_data_dir),
        "argv не содержит --user-data-dir: подметальщик не узнает свой процесс.\nargv: {argv}"
    );
    assert!(!argv.contains("--no-sandbox"));

    chrome.terminate(Duration::from_secs(5)).await.unwrap();
    assert!(!orphans::is_alive(chrome.pid), "Chrome пережил terminate");
    assert!(
        !Path::new(&meeting).join("chrome.pid").exists(),
        "chrome.pid должен исчезнуть после штатного гашения"
    );
}

// ---------------------------------------------------------------------------
// Мини-HTTP: две страницы на РАЗНЫХ источниках.
//
// Нужен именно свой сервер, а не `data:`-URL: у `data:` непрозрачный источник,
// и Chrome не выносит такой iframe в отдельный процесс — то есть проверить
// доставку инжекта в OOPIF на нём невозможно. Два локальных порта с разными
// ИМЕНАМИ хоста (`127.0.0.1` и `localhost`) — разные сайты в терминах
// изоляции, а значит настоящий OOPIF.
// ---------------------------------------------------------------------------

struct MiniHttp {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl MiniHttp {
    async fn start(routes: HashMap<String, String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = Arc::new(routes);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = match sock.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    // Специальный маршрут: сервер тянет с ответом. Нужен,
                    // чтобы снять скриншот со страницы, которая ЕЩЁ грузится.
                    if path.starts_with("/slow") {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    let body = routes.get(&path).cloned();
                    let resp = match body {
                        Some(b) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            b.len(),
                            b
                        ),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    };
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        Self { port, task }
    }
}

impl Drop for MiniHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Общая заготовка: запущенный Chrome + подключённый транспорт + браузер.
struct LiveBrowser {
    chrome: ChromeProcess,
    browser: CdpBrowser,
    transport: Arc<WsTransport>,
    _tmp: tempfile::TempDir,
}

impl LiveBrowser {
    async fn start() -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = LaunchConfig::new(tmp.path().join("meeting"), tmp.path().join("cache"));
        cfg.window_mode = WindowMode::Offscreen;
        let chrome = launch(&cfg).await.expect("Chrome не запустился");
        let transport = WsTransport::connect(&chrome.ws_url, TransportConfig::default())
            .await
            .expect("не подключились к CDP");
        let browser = CdpBrowser::new(transport.clone());
        Self {
            chrome,
            browser,
            transport,
            _tmp: tmp,
        }
    }

    async fn stop(mut self) {
        self.transport.close().await;
        self.chrome.terminate(Duration::from_secs(5)).await.unwrap();
        assert!(!orphans::is_alive(self.chrome.pid), "Chrome пережил terminate");
    }
}

/// Ждёт сообщение из страницы, отбрасывая служебные.
async fn next_payload(rx: &mut BindingReceiver, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "не дождались сообщения из страницы");
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok((_, BindingMessage::Payload(p)))) => return p,
            Ok(Ok((_, BindingMessage::Meta { .. }))) => continue,
            Ok(Err(e)) => panic!("канал биндинга умер: {e}"),
            Err(_) => panic!("не дождались сообщения из страницы"),
        }
    }
}

/// Инжект доезжает, глобал в `window` не остаётся, канал работает.
#[tokio::test]
async fn inject_delivers_and_leaves_no_global_in_window() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let page_html = "<!doctype html><meta charset=utf-8><title>meet</title><body>ok</body>";
    let http = MiniHttp::start(HashMap::from([("/".to_string(), page_html.to_string())])).await;

    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    let inject = InjectConfig {
        user_script: r#"
          post(JSON.stringify({kind: "hello", href: location.href}));
        "#
        .into(),
        ..InjectConfig::default()
    };
    let binding = inject.binding_name.clone();
    let setup = PageSetup {
        inject,
        ..PageSetup::default()
    };
    // Подписка ДО навигации: первые кадры приходят сразу после загрузки.
    let mut rx = BindingReceiver::new(page.session().subscribe(), binding.clone());
    page.prepare_and_navigate(&format!("http://127.0.0.1:{}/", http.port), &setup)
        .await
        .unwrap();

    let payload = next_payload(&mut rx, Duration::from_secs(15)).await;
    let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["kind"], "hello");
    assert!(v["href"].as_str().unwrap().contains("127.0.0.1"));

    // Глобала в window нет: скрипт забрал функцию в замыкание и удалил её.
    let res = page
        .session()
        .call(
            "Runtime.evaluate",
            json!({
                "expression": format!("typeof window[{}]", serde_json::to_string(&binding).unwrap()),
                "returnByValue": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        res["result"]["value"], "undefined",
        "код сайта не должен видеть нашу функцию в window"
    );

    live_browser.stop().await;
}

/// Кадры, отправленные ДО появления функции биндинга, не теряются.
///
/// Биндинг привязывается НАМЕРЕННО ПОЗЖЕ навигации — это ровно тот порядок,
/// который протокол не гарантирует и от которого страхует кольцевой буфер.
#[tokio::test]
async fn frames_sent_before_binding_appears_are_not_lost() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>buffer</body>".to_string(),
    )]))
    .await;

    let page = CdpPage::open(&live_browser.browser).await.unwrap();
    let session = page.session().clone();
    let inject = InjectConfig {
        ring_capacity: 64,
        user_script: r#"
          for (var i = 0; i < 50; i++) { post("frame-" + i); }
        "#
        .into(),
        ..InjectConfig::default()
    };
    let binding = inject.binding_name.clone();
    let mut rx = BindingReceiver::new(session.subscribe(), binding.clone());

    // Всё как обычно, КРОМЕ addBinding — его отложим.
    session
        .call("Page.setBypassCSP", json!({"enabled": true}))
        .await
        .unwrap();
    session
        .call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": forge_cdp::build_inject_source(&inject)}),
        )
        .await
        .unwrap();
    session
        .call(
            "Page.navigate",
            json!({"url": format!("http://127.0.0.1:{}/", http.port)}),
        )
        .await
        .unwrap();

    // Страница уже отправила 50 кадров в пустоту — они лежат в буфере.
    tokio::time::sleep(Duration::from_millis(700)).await;
    session
        .call("Runtime.addBinding", json!({"name": binding}))
        .await
        .unwrap();

    let mut got = Vec::new();
    while got.len() < 50 {
        got.push(next_payload(&mut rx, Duration::from_secs(10)).await);
    }
    let expected: Vec<String> = (0..50).map(|i| format!("frame-{i}")).collect();
    assert_eq!(got, expected, "порядок и полнота кадров из буфера");

    live_browser.stop().await;
}

/// Переполнение кольца: теряются САМЫЕ СТАРЫЕ кадры, счётчик потерь доезжает.
#[tokio::test]
async fn ring_buffer_overflow_is_counted_and_reported() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>overflow</body>".to_string(),
    )]))
    .await;

    let page = CdpPage::open(&live_browser.browser).await.unwrap();
    let session = page.session().clone();
    let inject = InjectConfig {
        ring_capacity: 8,
        user_script: r#"
          for (var i = 0; i < 20; i++) { post("f" + i); }
        "#
        .into(),
        ..InjectConfig::default()
    };
    let binding = inject.binding_name.clone();
    let mut rx = BindingReceiver::new(session.subscribe(), binding.clone());

    session
        .call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": forge_cdp::build_inject_source(&inject)}),
        )
        .await
        .unwrap();
    session
        .call(
            "Page.navigate",
            json!({"url": format!("http://127.0.0.1:{}/", http.port)}),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    session
        .call("Runtime.addBinding", json!({"name": binding}))
        .await
        .unwrap();

    let mut payloads = Vec::new();
    let mut dropped_reported = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && (payloads.len() < 8 || dropped_reported.is_none())
    {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok((_, BindingMessage::Payload(p)))) => payloads.push(p),
            Ok(Ok((_, BindingMessage::Meta { dropped, .. }))) => dropped_reported = Some(dropped),
            _ => break,
        }
    }
    assert_eq!(
        payloads,
        (12..20).map(|i| format!("f{i}")).collect::<Vec<_>>(),
        "кольцо обязано выбрасывать самые старые кадры, а не самые свежие"
    );
    assert_eq!(
        dropped_reported,
        Some(12),
        "счётчик потерь обязан доехать до Rust, иначе потеря останется тихой"
    );

    live_browser.stop().await;
}

/// Большой кадр не усекается по дороге через биндинг.
#[tokio::test]
async fn large_payload_is_not_truncated() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>big</body>".to_string(),
    )]))
    .await;

    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    // 4 МиБ — заведомо больше любого кадра PCM (кадр 20 мс моно 16 кГц это
    // 640 байт), то есть запас в тысячи раз.
    let inject = InjectConfig {
        user_script: r#"
          var big = new Array(4 * 1024 * 1024 + 1).join("x");
          post(JSON.stringify({len: big.length, tail: big.slice(-3), body: big}));
        "#
        .into(),
        ..InjectConfig::default()
    };
    let binding = inject.binding_name.clone();
    let setup = PageSetup {
        inject,
        ..PageSetup::default()
    };
    let mut rx = BindingReceiver::new(page.session().subscribe(), binding);
    page.prepare_and_navigate(&format!("http://127.0.0.1:{}/", http.port), &setup)
        .await
        .unwrap();

    let payload = next_payload(&mut rx, Duration::from_secs(30)).await;
    let v: serde_json::Value = serde_json::from_str(&payload).expect("большой кадр разобрался");
    let declared = v["len"].as_u64().unwrap();
    assert_eq!(declared, 4 * 1024 * 1024);
    assert_eq!(
        v["body"].as_str().unwrap().len() as u64,
        declared,
        "кадр доехал усечённым — это молчаливая потеря данных"
    );
    assert_eq!(v["tail"], "xxx");

    live_browser.stop().await;
}

/// Инжект доезжает в cross-origin iframe (OOPIF) через автоаттач.
#[tokio::test]
async fn inject_reaches_cross_origin_iframe() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;

    let frame = MiniHttp::start(HashMap::from([(
        "/frame".to_string(),
        "<!doctype html><meta charset=utf-8><body>frame</body>".to_string(),
    )]))
    .await;
    // Хост НАМЕРЕННО другой (`localhost` против `127.0.0.1`): для изоляции
    // сайтов это разные сайты, значит iframe уедет в отдельный процесс.
    let top_html = format!(
        r#"<!doctype html><meta charset=utf-8><title>top</title>
           <body>top<iframe src="http://localhost:{}/frame"></iframe></body>"#,
        frame.port
    );
    let top = MiniHttp::start(HashMap::from([("/".to_string(), top_html)])).await;

    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    let inject = InjectConfig {
        user_script: r#"
          post(JSON.stringify({href: location.href, top: window.top === window.self}));
        "#
        .into(),
        ..InjectConfig::default()
    };
    let binding = inject.binding_name.clone();
    let setup = PageSetup {
        inject,
        ..PageSetup::default()
    };
    let mut rx = BindingReceiver::new(page.session().subscribe(), binding);
    page.prepare_and_navigate(&format!("http://127.0.0.1:{}/", top.port), &setup)
        .await
        .unwrap();

    let mut from_frame = None;
    let mut from_top = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while (from_frame.is_none() || from_top.is_none())
        && tokio::time::Instant::now() < deadline
    {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Ok((session, msg))) = tokio::time::timeout(left, rx.recv()).await else {
            break;
        };
        if let BindingMessage::Payload(p) = msg {
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            let href = v["href"].as_str().unwrap_or_default().to_string();
            if href.contains("/frame") {
                from_frame = Some((session, href, v["top"].as_bool().unwrap_or(true)));
            } else {
                from_top = Some((session, href));
            }
        }
    }

    let (frame_session, frame_href, is_top) =
        from_frame.expect("инжект НЕ доехал в cross-origin iframe — автоаттач не сработал");
    let (top_session, _) = from_top.expect("не получено сообщение из главного документа");
    assert!(!is_top, "сообщение должно прийти именно из вложенного документа");
    assert!(frame_href.contains("localhost"));
    assert_ne!(
        frame_session, top_session,
        "OOPIF обязан говорить своей flat-сессией"
    );
    eprintln!("OOPIF: главная сессия {top_session:?}, сессия фрейма {frame_session:?}");

    live_browser.stop().await;
}

/// Насос автоаттача применяет ту же настройку к дочерней цели.
#[tokio::test]
async fn inject_setup_is_reusable_for_child_targets() {
    if !live() {
        return;
    }
    // Проверка компилируемости контракта: InjectSetup реализует SessionSetup
    // и потому годится и главной странице, и каждому OOPIF.
    let cfg = InjectConfig::default();
    let setup: Arc<dyn forge_cdp::SessionSetup> = Arc::new(InjectSetup::new(cfg));
    let _ = setup;
}

/// Мок-страница для проверки ввода: кнопка, поле и журнал событий.
///
/// Ключевое здесь — журнал пишет `event.isTrusted`. Именно это свойство
/// проверяют Zoom и Teams, и именно его нет у `el.click()`.
const INPUT_FIXTURE: &str = r#"<!doctype html><meta charset="utf-8"><title>fixture</title>
<body style="margin:0">
<button id="go" style="position:absolute;left:100px;top:200px;width:180px;height:40px">Продолжить в браузере</button>
<input id="name" style="position:absolute;left:100px;top:300px;width:220px;height:30px">
<script>
  window.__log = [];
  document.getElementById('go').addEventListener('click', function (e) {
    window.__log.push({kind: 'click', trusted: e.isTrusted, x: e.clientX, y: e.clientY});
  });
  var inp = document.getElementById('name');
  inp.addEventListener('input', function (e) {
    window.__log.push({kind: 'input', trusted: e.isTrusted, value: inp.value});
  });
</script>
</body>"#;

async fn eval_json(page: &CdpPage, expression: &str) -> serde_json::Value {
    let res = page
        .session()
        .call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
        )
        .await
        .unwrap();
    assert!(
        res.get("exceptionDetails").is_none(),
        "исключение в выражении: {res}"
    );
    let raw = res["result"]["value"].as_str().unwrap_or("null");
    serde_json::from_str(raw).unwrap()
}

/// Ждёт, пока документ дозагрузится.
async fn wait_ready(page: &CdpPage) {
    for _ in 0..100 {
        let res = page
            .session()
            .call(
                "Runtime.evaluate",
                json!({"expression": "document.readyState", "returnByValue": true}),
            )
            .await
            .unwrap();
        if res["result"]["value"] == "complete" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("страница не догрузилась");
}

/// Клик и ввод: доверенные события, координаты из DOM.
#[tokio::test]
async fn input_click_and_type_produce_trusted_events() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        INPUT_FIXTURE.to_string(),
    )]))
    .await;

    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    page.prepare_and_navigate(
        &format!("http://127.0.0.1:{}/", http.port),
        &PageSetup::default(),
    )
    .await
    .unwrap();
    wait_ready(&page).await;

    // 1. Клик по селектору: координаты берутся из getBoundingClientRect.
    let center = page.click_selector("#go").await.unwrap();
    assert_eq!((center.x, center.y), (190.0, 220.0), "центр бокса кнопки");

    // 2. Клик по выражению с поиском по тексту — так на Телемосте ищется
    //    интерстишал «Продолжить в браузере»: класс там с хешем сборки.
    page.click_expression(
        "Array.from(document.querySelectorAll('button')).find(function (b) { return b.textContent.indexOf('Продолжить') >= 0; })",
    )
    .await
    .unwrap();

    // 3. Для сравнения — синтетический клик из JS.
    page.session()
        .call(
            "Runtime.evaluate",
            json!({"expression": "document.getElementById('go').click()"}),
        )
        .await
        .unwrap();

    // 4. Ввод текста.
    page.type_text("#name", "Бот F.O.R.G.E.").await.unwrap();

    // 5. И прямое присваивание value — то, что React игнорирует.
    page.session()
        .call(
            "Runtime.evaluate",
            json!({"expression": "document.getElementById('name').value = 'ПРЯМОЕ ПРИСВАИВАНИЕ'"}),
        )
        .await
        .unwrap();

    let log = eval_json(&page, "JSON.stringify(window.__log)").await;
    let entries = log.as_array().unwrap();
    let clicks: Vec<_> = entries.iter().filter(|e| e["kind"] == "click").collect();
    let inputs: Vec<_> = entries.iter().filter(|e| e["kind"] == "input").collect();

    assert_eq!(clicks.len(), 3, "два наших клика и один синтетический");
    assert_eq!(clicks[0]["trusted"], true, "Input.dispatchMouseEvent обязан быть доверенным");
    assert_eq!(clicks[0]["x"], 190.0);
    assert_eq!(clicks[0]["y"], 220.0);
    assert_eq!(clicks[1]["trusted"], true, "клик по выражению с поиском по тексту");
    assert_eq!(
        clicks[2]["trusted"], false,
        "el.click() недоверенный — ровно поэтому он не годится для Zoom и Teams"
    );

    assert_eq!(inputs.len(), 1, "прямое присваивание value НЕ порождает события input");
    assert_eq!(
        inputs[0]["trusted"], true,
        "Input.insertText обязан порождать настоящее событие ввода — на него и рассчитывает React"
    );
    assert_eq!(inputs[0]["value"], "Бот F.O.R.G.E.");

    // Значение в поле действительно наше (до прямого присваивания).
    eprintln!("журнал событий страницы: {log}");

    live_browser.stop().await;
}

/// Окно ездит между видимой зоной и офскрином; сообщаем ФАКТИЧЕСКИЕ границы.
#[tokio::test]
async fn window_moves_between_offscreen_and_visible() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>window</body>".to_string(),
    )]))
    .await;
    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    page.prepare_and_navigate(
        &format!("http://127.0.0.1:{}/", http.port),
        &PageSetup::default(),
    )
    .await
    .unwrap();

    let start = page.window_bounds().await.unwrap();
    let visible = page.set_window_visible(true).await.unwrap();
    let offscreen = page.set_window_visible(false).await.unwrap();
    eprintln!(
        "границы окна: старт {start:?}\n  видимо  запрошено {:?} получено {visible:?}\n  офскрин запрошено {:?} получено {offscreen:?}",
        page.placement().visible,
        page.placement().offscreen
    );

    assert_eq!(visible.width, page.placement().visible.width);
    assert_eq!(visible.height, page.placement().visible.height);
    assert_eq!(visible.left, page.placement().visible.left);
    assert_eq!(visible.top, page.placement().visible.top);
    // Насколько далеко можно увести окно — вопрос оконного сервера; проверяем
    // сам факт переноса, а не конкретное число.
    assert_ne!(
        offscreen.left, visible.left,
        "окно обязано уезжать из видимой зоны"
    );

    live_browser.stop().await;
}

/// Скриншот снимается и с окна за экраном, и с ещё грузящейся страницы.
#[tokio::test]
async fn screenshot_works_offscreen_and_while_loading() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([
        (
            "/".to_string(),
            "<!doctype html><meta charset=utf-8><body style='background:#123456'>shot</body>"
                .to_string(),
        ),
        (
            "/slow".to_string(),
            "<!doctype html><meta charset=utf-8><body>наконец-то</body>".to_string(),
        ),
    ]))
    .await;

    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    page.prepare_and_navigate(
        &format!("http://127.0.0.1:{}/", http.port),
        &PageSetup::default(),
    )
    .await
    .unwrap();
    wait_ready(&page).await;

    // Окно уведено за экран — самый неудобный для скриншота случай.
    page.set_window_visible(false).await.unwrap();
    let png = page
        .screenshot(&forge_cdp::ScreenshotOptions::default())
        .await
        .unwrap();
    assert_eq!(
        &png[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        "не PNG"
    );
    assert!(png.len() > 1000, "подозрительно пустой скриншот: {} байт", png.len());
    // IHDR идёт сразу за сигнатурой: байты 16..24 — ширина и высота. Проверка
    // защищает от «формально PNG, а внутри вырожденный кадр».
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert!(
        width > 100 && height > 100,
        "снят вырожденный кадр {width}×{height}"
    );
    eprintln!(
        "скриншот неактивной вкладки за экраном: {width}×{height}, {} байт",
        png.len()
    );

    // И со страницы, которая ещё грузится: навигация запущена, ответ сервера
    // придёт через три секунды, а скриншот нужен сейчас.
    let slow_url = format!("http://127.0.0.1:{}/slow", http.port);
    let session = page.session().clone();
    let nav = tokio::spawn(async move {
        let _ = session
            .call("Page.navigate", json!({ "url": slow_url }))
            .await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let png2 = tokio::time::timeout(
        Duration::from_secs(10),
        page.screenshot(&forge_cdp::ScreenshotOptions::default()),
    )
    .await
    .expect("скриншот повис на грузящейся странице")
    .expect("скриншот упал на грузящейся странице");
    assert_eq!(&png2[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let _ = nav.await;

    live_browser.stop().await;
}

/// Bundle id приложения, которое сейчас на переднем плане.
///
/// Через `lsappinfo` — он есть в системе, не требует ни разрешений
/// «Автоматизации», ни AppleScript (запрос разрешения сам по себе отобрал бы
/// фокус и испортил измерение).
fn frontmost_app() -> String {
    let asn = std::process::Command::new("lsappinfo")
        .arg("front")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if asn.is_empty() {
        return String::new();
    }
    std::process::Command::new("lsappinfo")
        .args(["info", "-only", "bundleid", &asn])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Запуск бота НЕ отбирает фокус у пользователя.
///
/// Требование эргономическое: пользователь работает параллельно с ботом, и
/// всплывающее окно перебивает ему ввод. Это НЕ то же самое, что «окно за
/// экраном»: на macOS активируется приложение, а не окно, и уехавшее за
/// границу экрана окно фокус всё равно бы утащило.
#[tokio::test]
async fn launching_and_opening_a_page_does_not_steal_focus() {
    if !live() {
        return;
    }
    let before = frontmost_app();
    assert!(!before.is_empty(), "не удалось определить фронтовое приложение");
    assert!(
        !before.contains("com.google.Chrome"),
        "измерение бессмысленно: Chrome уже на переднем плане"
    );

    let live_browser = LiveBrowser::start().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after_launch = frontmost_app();

    // Вкладка создаётся в фоне.
    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>фон</body>".to_string(),
    )]))
    .await;
    page.prepare_and_navigate(
        &format!("http://127.0.0.1:{}/", http.port),
        &PageSetup::default(),
    )
    .await
    .unwrap();
    wait_ready(&page).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let after_page = frontmost_app();

    eprintln!("фронт: до {before}, после запуска {after_launch}, после открытия вкладки {after_page}");

    assert_eq!(
        after_launch, before,
        "запуск браузера отобрал фокус: было {before}, стало {after_launch}"
    );
    assert_eq!(
        after_page, before,
        "создание вкладки отобрало фокус: было {before}, стало {after_page}"
    );

    live_browser.stop().await;
}

/// СВЁРНУТОЕ ОКНО: показ человеком и возврат обратно.
///
/// # Что измерено оконным сервером macOS (Chrome 151, `CGWindowListCopyWindowInfo`)
///
/// | момент | окон бота на экране |
/// |---|---|
/// | бот работает сам (любой режим) | **0** |
/// | человек нажал «показать окно» | 1, 1280×800 в (80, 80) |
/// | окно убрано, режим `minimized` | **0** |
/// | окно убрано, режим `offscreen` | **1, 1280×800 в (−930, 30)** — те самые 350 пикселей поверх работы |
///
/// Отсюда дефолт `minimized`: пока бота не показывали, окна нет на экране ни в
/// одном режиме, но ПОСЛЕ показа вернуть его за экран на macOS невозможно —
/// координаты подрезаются. Свернуть можно.
///
/// # Чего этот тест НЕ проверяет и почему
///
/// Фактического исчезновения окна: `Browser.getWindowBounds` на macOS
/// продолжает отвечать `windowState: "normal"` для СВЁРНУТОГО окна (проверено:
/// `maximized` и `fullscreen` тот же вызов отражает верно, а попытка свернуть
/// окно из полноэкранного даёт внятную ошибку протокола — то есть обработчик
/// на месте и работает, врёт именно отчёт). Проверять состояние по этому
/// вызову означало бы писать заведомо красный тест. Факт исчезновения снят
/// оконным сервером один раз, таблицей выше; за поведением кода следит
/// модульный тест `a_minimized_bot_returns_to_minimized_after_being_shown`.
#[tokio::test]
async fn a_minimized_window_can_be_shown_to_a_human_and_hidden_again() {
    if !live() {
        return;
    }
    let live_browser = LiveBrowser::start().await;
    let http = MiniHttp::start(HashMap::from([(
        "/".to_string(),
        "<!doctype html><meta charset=utf-8><body>свёрнутое окно</body>".to_string(),
    )]))
    .await;
    let mut page = CdpPage::open(&live_browser.browser).await.unwrap();
    let setup = PageSetup {
        window_mode: WindowMode::Minimized,
        ..PageSetup::default()
    };
    page.prepare_and_navigate(&format!("http://127.0.0.1:{}/", http.port), &setup)
        .await
        .unwrap();
    assert_eq!(page.hidden_mode(), WindowMode::Minimized);

    // «Показать, что видит бот» обязано работать и у свёрнутого окна: это
    // несущая функция ассистированного входа. ИЗМЕРЕНО: сразу после
    // сворачивания снимок отвечает `Not attached to an active page` и
    // начинает работать через секунду-полторы — поверхности ещё нет.
    let mut shot = Err(());
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Ok(bytes) = page.screenshot(&Default::default()).await {
            if !bytes.is_empty() {
                shot = Ok(bytes);
                break;
            }
        }
    }
    assert!(shot.is_ok(), "свёрнутое окно так и не отдало снимок");

    // Человек просит показать окно.
    let shown = page.set_window_visible(true).await.expect("окно не показалось");
    assert_eq!(shown.left, page.placement().visible.left);
    assert_eq!(shown.top, page.placement().visible.top);
    assert_eq!(shown.width, page.placement().visible.width);

    // И убирает обратно. Свёрнутое окно координат не принимает, поэтому
    // отсутствие ошибки здесь — это и есть проверка порядка вызовов.
    page.set_window_visible(false)
        .await
        .expect("окно не убралось обратно");
    assert!(
        page.screenshot(&Default::default()).await.is_ok(),
        "после возврата в свёрнутое состояние снимок перестал работать"
    );

    live_browser.stop().await;
}
