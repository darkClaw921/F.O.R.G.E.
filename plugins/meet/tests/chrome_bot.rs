//! Боевой адаптер: порядок команд CDP без браузера и живой прогон на
//! мок-встрече.
//!
//! # Две половины файла проверяют разное
//!
//! **Герметичная** идёт всегда и отвечает на вопрос «в каком порядке ушли
//! команды». Порядок `setBypassCSP` → биндинг → скрипт → `setAutoAttach` →
//! `navigate` ломается МОЛЧА: страница грузится, вход проходит, а звука нет —
//! потому что `RTCPeerConnection` площадки создан раньше нашего патча. Ни один
//! другой тест этого не заметит.
//!
//! **Живая** идёт за `FORGE_MEET_E2E=1` и отвечает на вопрос «работает ли
//! вход». Мок-встреча запускается с `autojoin=0`, то есть ведёт себя как
//! настоящая площадка: пока бот не нажмёт «Продолжить в браузере», не введёт
//! имя и не нажмёт «Присоединиться» — встречи не будет, а значит не будет ни
//! дорожек, ни звука. Проверка входа получается неподдельной: сам факт
//! пришедшего PCM означает, что бот вошёл.
//!
//! ```bash
//! FORGE_MEET_E2E=1 cargo test -p forge-meet --test chrome_bot -- --nocapture
//! ```

use std::time::Duration;

use forge_cdp::page::CdpPage;
use forge_cdp::session::CdpBrowser;
use forge_cdp::testing::RecordingTransport;
use forge_meet::bot::chrome::{ChromeBot, ChromeBotConfig, ChromeBotFactory, BotTimings};
use forge_meet::bot::platforms::{ProbeBook, MOCK_PLATFORM};
use forge_meet::bot::{inject, EndConfig, MeetBot};
use forge_meet::config::{MeetConfig, WindowMode};
use forge_meet::hub::event::{MeetEvent, StopReason};
use forge_meet::testing::{e2e_enabled, mock_meeting_dir, StaticServer};
use forge_meet::JoinMode;

fn mock_config(dir: &std::path::Path, url: &str) -> ChromeBotConfig {
    let mut cfg = MeetConfig {
        config_path: dir.join("meet.toml"),
        cache_dir: dir.join("cache"),
        db_path: dir.join("cache/meet.db"),
        models_dir: dir.join("cache/models"),
        ..MeetConfig::default()
    };
    // Прогон идёт в ДЕФОЛТНОМ режиме окна (`minimized`, выбран измерением в
    // Ф3): держать живой тест на другом режиме значило бы проверять не то, что
    // работает у пользователя.
    cfg.window_mode = WindowMode::default();

    ChromeBotConfig {
        url: url.to_string(),
        meeting_dir: dir.join("meeting"),
        cache_dir: cfg.cache_dir.clone(),
        bot_name: "F.O.R.G.E.".to_string(),
        probes: ProbeBook::builtin().for_platform(MOCK_PLATFORM),
        inject: inject::inject_config(&cfg),
        sample_rate: cfg.audio_sample_rate,
        window_mode: cfg.window_mode,
        chrome_path: None,
        ffmpeg_path: None,
        mute_audio: cfg.mute_audio,
        cdp_command_timeout_ms: cfg.cdp_command_timeout_ms,
        persist_profile: false,
        end: EndConfig::from(&cfg),
        timings: BotTimings {
            join_timeout_ms: 20_000,
            step_settle_ms: 250,
            observe_interval_ms: 300,
            leave_grace_ms: 400,
            terminate_grace_ms: 4_000,
            observe_failures_to_give_up: 3,
            join_step_attempts: 3,
            join_audio_attempts: 4,
        },
    }
}

// ---------------------------------------------------------------------------
// Герметичная половина
// ---------------------------------------------------------------------------

/// ПОРЯДОК КОМАНД ВХОДА. Инжект и биндинг стоят ДО навигации.
#[tokio::test]
async fn the_injection_and_the_binding_go_before_the_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = mock_config(dir.path(), "https://telemost.yandex.ru/j/60497752704486");

    let t = RecordingTransport::new();
    let browser = CdpBrowser::new(t.clone());
    let session = browser.open_target("about:blank").await.unwrap();
    let mut page = CdpPage::new(browser, session);
    t.clear();

    page.prepare_and_navigate(&cfg.url, &cfg.page_setup(JoinMode::Normal))
        .await
        .unwrap();

    let methods = t.methods();
    let idx = |m: &str| {
        methods
            .iter()
            .position(|x| x == m)
            .unwrap_or_else(|| panic!("команда {m} не отправлена вовсе: {methods:?}"))
    };
    let navigate = idx("Page.navigate");
    for before in [
        "Page.setBypassCSP",
        "Browser.grantPermissions",
        "Runtime.addBinding",
        "Page.addScriptToEvaluateOnNewDocument",
        "Target.setAutoAttach",
    ] {
        assert!(
            idx(before) < navigate,
            "{before} ушла ПОСЛЕ Page.navigate — инжект опоздал, звука не будет: {methods:?}"
        );
    }
    assert!(
        idx("Page.setBypassCSP") < idx("Runtime.addBinding"),
        "CSP снимается первым: Ф0 измерила, что инлайн-скрипт на Телемосте блокируется"
    );

    // И в страницу уехал ИМЕННО НАШ скрипт захвата, а не пустая обёртка.
    let script = t.calls_of("Page.addScriptToEvaluateOnNewDocument")[0].params["source"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(script.contains("installPeerConnectionTap"), "в странице нет тапа WebRTC");
    assert!(script.contains("installWebAudioTap"), "в странице нет тапа Web Audio");
    assert!(
        script.contains(&cfg.inject.binding_name),
        "скрипт не знает имени биндинга — кадры некуда отдавать"
    );

    // Навигация идёт на адрес встречи, а не на about:blank.
    assert_eq!(
        t.calls_of("Page.navigate")[0].params["url"].as_str(),
        Some(cfg.url.as_str())
    );
}

/// Имя биндинга СЛУЧАЙНОЕ и своё на каждую встречу.
///
/// Постоянное имя было бы, во-первых, готовым признаком автоматизации для
/// антибот-логики площадки, во-вторых, риском столкновения двух ботов в одном
/// профиле.
#[test]
fn every_meeting_gets_its_own_binding_name() {
    let dir = tempfile::tempdir().unwrap();
    let a = mock_config(dir.path(), "https://telemost.yandex.ru/j/1");
    let b = mock_config(dir.path(), "https://telemost.yandex.ru/j/1");
    assert_ne!(a.inject.binding_name, b.inject.binding_name);
    assert!(a.inject.binding_name.starts_with("__forge"), "{}", a.inject.binding_name);
}

/// Каналы отдаются РОВНО ОДИН РАЗ: второй `start` не имеет права молча
/// подменить поток событий.
#[tokio::test]
async fn the_streams_are_handed_out_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    // Chrome заведомо нет по этому пути — старт всё равно обязан отдать каналы
    // и сообщить о беде событием, а не отказом вызова.
    let mut cfg = mock_config(dir.path(), "https://telemost.yandex.ru/j/1");
    cfg.chrome_path = Some(dir.path().join("нет-такого-chrome"));
    let bot = ChromeBot::new(cfg);

    let mut streams = bot.start(JoinMode::Normal).await.unwrap();
    assert!(bot.start(JoinMode::Normal).await.is_err(), "каналы отданы дважды");

    // ОТСУТСТВИЕ CHROME — ЭТО КОД ЛЕСТНИЦЫ ОШИБОК, А НЕ ПАНИКА.
    let mut kinds = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), streams.events.recv()).await {
            Ok(Some(ev)) => {
                let terminal = ev.is_terminal();
                if let MeetEvent::Failed { code, .. } = &ev {
                    kinds.push(code.clone());
                }
                if terminal {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    assert_eq!(kinds, vec!["chrome_unavailable"], "провал запуска обязан быть назван");
    assert!(bot.is_finished(), "задача обязана закончиться, а не висеть");
    bot.stop(StopReason::UserStop).await.unwrap();
}

/// Фабрика подставляет пробы платформы и переживает незнакомый адрес.
#[test]
fn the_factory_builds_a_bot_for_a_known_platform_and_for_an_unknown_one() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MeetConfig {
        cache_dir: dir.path().join("cache"),
        ..MeetConfig::default()
    };
    let factory = ChromeBotFactory::new(cfg);
    assert!(factory.probes().platforms().contains(&"telemost"));

    for url in [
        "https://telemost.yandex.ru/j/60497752704486",
        "https://неизвестная.площадка/room/1",
    ] {
        let req = forge_meet::BotRequest {
            meeting_id: "m1".into(),
            url: url.to_string(),
            mode: JoinMode::Normal,
            meeting_dir: dir.path().join("m1"),
            bot_name: "F.O.R.G.E.".into(),
        };
        use forge_meet::BotFactory as _;
        assert!(factory.create(&req).is_ok(), "фабрика отказала на {url}");
    }
}

// ---------------------------------------------------------------------------
// Живая половина
// ---------------------------------------------------------------------------

/// Собирает события бота до терминального либо до конца отпущенного времени.
///
/// Возвращает `true`, если поток кончился терминальным событием.
async fn collect(
    streams: &mut forge_meet::BotStreams,
    limit: Duration,
    events: &mut Vec<MeetEvent>,
    frames: &mut usize,
) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        tokio::select! {
            ev = tokio::time::timeout(left, streams.events.recv()) => match ev {
                Ok(Some(ev)) => {
                    let terminal = ev.is_terminal();
                    events.push(ev);
                    if terminal { return true; }
                }
                Ok(None) => return true,
                Err(_) => return false,
            },
            frame = streams.audio.recv() => {
                if frame.is_some() { *frames += 1; }
            }
        }
    }
}

/// Спрашивает у самой мок-встречи, что с ней сделали.
async fn ask_page(bot: &ChromeBot, expr: &str) -> serde_json::Value {
    bot.evaluate_json(expr).await.unwrap_or(serde_json::Value::Null)
}

/// ЖИВОЙ ВХОД: бот сам проходит интерстишал, вводит имя и попадает во встречу.
///
/// Страница поднимается с `autojoin=0`, поэтому без кликов бота встреча не
/// начнётся вовсе — ни дорожек, ни звука. Пришедший PCM и есть доказательство
/// входа.
#[tokio::test]
async fn the_bot_joins_the_mock_meeting_by_itself_and_records() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    // Встреча идёт двенадцать секунд: тесту нужно время СПРОСИТЬ САМУ
    // СТРАНИЦУ, что с ней сделали, пока браузер ещё жив. После выхода бота
    // страницы не существует, и спрашивать некого — на этом первая версия
    // теста и споткнулась.
    let url = server.mock_meeting_url("per-track", "autojoin=0&duration=12000");
    let bot = ChromeBot::new(mock_config(root.path(), &url));

    let mut streams = bot.start(JoinMode::Normal).await.unwrap();
    let mut events = Vec::new();
    let mut frames = 0usize;

    // Фаза 1: вход и первые секунды записи, страница ещё жива.
    collect(&mut streams, Duration::from_secs(8), &mut events, &mut frames).await;
    let kinds: Vec<&str> = events.iter().map(MeetEvent::kind).collect();
    eprintln!("[фаза 1] события: {kinds:?}, кадров {frames}");
    assert!(kinds.contains(&"joined"), "бот не вошёл: {kinds:?}");

    // 1. Вход состоялся — и это видно СТРАНИЦЕ, а не только нам.
    let clicks = ask_page(&bot, "JSON.stringify(window.__mockMeeting.clicks)")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    let name = ask_page(&bot, "window.__mockMeeting.displayName")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        clicks.contains("continue-in-browser") && clicks.contains("join"),
        "страница не увидела кликов входа: {clicks}"
    );
    assert_eq!(name, "F.O.R.G.E.", "имя бота не введено в поле");

    // 2. МИКРОФОН И КАМЕРА ВЫКЛЮЧЕНЫ — второй барьер поверх файла тишины.
    let mic = ask_page(&bot, "window.__mockMeeting.micEnabled").await;
    let cam = ask_page(&bot, "window.__mockMeeting.camEnabled").await;
    assert_eq!(mic, serde_json::json!(false), "микрофон остался включённым: бота будет слышно");
    assert_eq!(cam, serde_json::json!(false), "камера осталась включённой");

    // Фаза 2: досматриваем встречу до конца.
    let ended = collect(&mut streams, Duration::from_secs(25), &mut events, &mut frames).await;
    let kinds: Vec<&str> = events.iter().map(MeetEvent::kind).collect();
    eprintln!("[фаза 2] события: {kinds:?}, кадров {frames}");
    assert!(ended, "поток событий не закончился: {kinds:?}");

    // 3. КОНТРАКТ ПОРЯДКА соблюдён ЖИВЫМ ботом, а не только сценарным.
    forge_meet::hub::event::validate_order(&events)
        .unwrap_or_else(|e| panic!("живой бот нарушил контракт порядка: {e}\n{kinds:?}"));

    // 4. Запись пошла: дорожки, звук, путь захвата.
    assert!(frames > 0, "через биндинг не пришло ни одного кадра: {kinds:?}");
    assert!(kinds.contains(&"recording_started"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "track_added").count(),
        3,
        "мок-встреча в режиме per-track даёт ровно три дорожки: {kinds:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, MeetEvent::CapturePathDetected { path } if path == "rtcpeerconnection")),
        "путь захвата не тот: {kinds:?}"
    );

    // 5. УЧАСТНИКИ ИЗ DOM. Их четверо: бот и трое собеседников.
    let participants = events
        .iter()
        .rev()
        .find_map(|e| match e {
            MeetEvent::ParticipantsChanged { participants, .. } => Some(participants.clone()),
            _ => None,
        })
        .expect("список участников не приехал ни разу");
    assert_eq!(participants.len(), 4, "участники: {participants:?}");
    let names: Vec<&str> = participants.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Алиса") && names.contains(&"F.O.R.G.E."), "{names:?}");

    // 6. АКТИВНЫЙ СПИКЕР меняется по ходу встречи и попадает на общую ось.
    let speakers: Vec<(u64, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            MeetEvent::ActiveSpeaker { at_ms, name } => Some((*at_ms, name.clone())),
            _ => None,
        })
        .collect();
    assert!(speakers.len() >= 2, "смены активного спикера не зафиксированы: {speakers:?}");
    assert!(
        speakers.iter().any(|(at, n)| *at > 0 && n.is_some()),
        "у смен спикера нет меток на общей оси: {speakers:?}"
    );
    let heard: std::collections::BTreeSet<&str> =
        speakers.iter().filter_map(|(_, n)| n.as_deref()).collect();
    assert!(heard.len() >= 2, "говорил кто-то один — DOM-индикатор не читается: {heard:?}");

    // 7. БОТ ВЫШЕЛ САМ по независимому сигналу, а не по нашей команде.
    let stopped = events.iter().find_map(|e| match e {
        MeetEvent::RecordingStopped { reason, .. } => Some(*reason),
        _ => None,
    });
    eprintln!("причина остановки: {stopped:?}");
    assert!(
        matches!(
            stopped,
            Some(StopReason::AllTracksEnded | StopReason::DomEnded | StopReason::Silence)
        ),
        "бот не заметил конца встречи: {stopped:?}"
    );

    // 8. ЖИВОГО CHROME НЕ ОСТАЛОСЬ.
    bot.stop(StopReason::UserStop).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let profile = root
        .path()
        .join("meeting/chrome-profile")
        .to_string_lossy()
        .to_string();
    let leftovers = std::process::Command::new("ps")
        .args(["-ax", "-o", "args="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains(&profile))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "после выхода остался живой Chrome с нашим профилем");
}

/// ИНВАРИАНТ АССИСТИРОВАННОГО ВХОДА НА ЖИВОМ БРАУЗЕРЕ: ни одного клика, пока
/// человек не скажет.
///
/// Герметичный тест проверяет то же самое на `ScriptedBot`, но подмена бота
/// сценарием как раз и не отвечает на вопрос, кликает ли БОЕВОЙ адаптер.
/// Здесь спрашивают саму страницу.
#[tokio::test]
async fn assisted_mode_clicks_nothing_on_a_real_page() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url("per-track", "autojoin=0&duration=6000");
    let mut cfg = mock_config(root.path(), &url);
    // Ассистированный вход показывает окно человеку — это его смысл.
    cfg.window_mode = WindowMode::Visible;
    let bot = ChromeBot::new(cfg);

    let mut streams = bot.start(JoinMode::Assisted).await.unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;

    let clicks = ask_page(&bot, "JSON.stringify(window.__mockMeeting.clicks)")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(clicks, "[]", "бот кликал сам: {clicks}");
    assert_eq!(bot.click_count(), 0, "{:?}", bot.actions());

    // Встречи нет вовсе: без человека бот её не начинал.
    let joined = ask_page(&bot, "window.__mockMeeting.joined").await;
    assert_eq!(joined, serde_json::json!(false), "запись пошла без человека");

    // Человек вошёл сам — и только потом жмёт «я внутри».
    ask_page(&bot, "(window.__mockMeeting.start(), 1)").await;
    bot.confirm_human_ready().await.unwrap();

    let mut events = Vec::new();
    let mut frames = 0usize;
    collect(&mut streams, Duration::from_secs(20), &mut events, &mut frames).await;
    let kinds: Vec<&str> = events.iter().map(MeetEvent::kind).collect();
    eprintln!("ассистированный вход: {kinds:?}, кадров {frames}");
    assert!(kinds.contains(&"joined"), "после подтверждения запись обязана пойти: {kinds:?}");
    assert!(frames > 0, "звук не пошёл: {kinds:?}");

    // ЗА ЧЕЛОВЕКА БОТ НЕ ВХОДИТ И ПОСЛЕ ПОДТВЕРЖДЕНИЯ: кнопок входа он не
    // касался ни разу за весь прогон. А вот микрофон гасит — в ассистированном
    // режиме тоже, иначе бот был бы слышен во встрече, куда его завёл человек.
    let entered: Vec<String> = bot
        .actions()
        .iter()
        .filter_map(|a| match a {
            forge_meet::bot::scripted::BotAction::Click(what)
                if what.contains("join") || what.contains("continue") =>
            {
                Some(what.clone())
            }
            forge_meet::bot::scripted::BotAction::Type(_) => Some("type".to_string()),
            _ => None,
        })
        .collect();
    assert!(entered.is_empty(), "бот вошёл сам в ассистированном режиме: {entered:?}");

    bot.stop(StopReason::UserStop).await.unwrap();
}

// ---------------------------------------------------------------------------
// Живая половина: механизмы фаз 8–10
// ---------------------------------------------------------------------------
//
// ЧЕГО ЭТИ ТЕСТЫ НЕ ДОКАЗЫВАЮТ. Они НЕ проверяют селекторы Google Meet, Teams
// и Zoom — живых встреч на этих площадках у нас не было, и все их пробы в
// `probes.toml` помечены черновиком. Проверяется РОВНО ТО, что проверить
// можно: сами механизмы, которые фазы 8–10 принесли, работают в НАСТОЯЩЕМ
// Chrome, а не только на фейковом DOM. Между «логика верна на структурах» и
// «работает в браузере» за этот проект уже трижды обнаруживалась разница.

/// ОТКАЗ ВО ВХОДЕ доезжает до `needs_human` живьём.
///
/// Мок с `deny=1` ведёт себя как Google Meet с ограниченным доступом: вместо
/// встречи показывает текст отказа, и сколько ни жми — ничего не изменится.
/// Ожидание здесь стоило бы минуты молчания, после которой человек получил бы
/// причину `unknown` и искал бы дефект там, где его нет.
#[tokio::test]
async fn a_denied_join_asks_for_a_human_with_the_real_reason() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url("per-track", "autojoin=0&deny=1&duration=20000");
    let bot = ChromeBot::new(mock_config(root.path(), &url));

    let mut streams = bot.start(JoinMode::Normal).await.unwrap();
    let mut events = Vec::new();
    let mut frames = 0usize;
    collect(&mut streams, Duration::from_secs(10), &mut events, &mut frames).await;

    let kinds: Vec<&str> = events.iter().map(MeetEvent::kind).collect();
    eprintln!("[отказ] события: {kinds:?}");
    let kind = events
        .iter()
        .find_map(|e| match e {
            MeetEvent::NeedsHuman { kind } => Some(kind.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert_eq!(kind, "denied", "причина обязана быть названа, а не «unknown»: {kinds:?}");
    assert!(
        !kinds.contains(&"joined"),
        "бот считает себя вошедшим на экране отказа: {kinds:?}"
    );

    // И это заняло СЕКУНДЫ, а не минуту таймаута входа: у отказа отдельный
    // шаг именно ради этого.
    bot.stop(StopReason::UserStop).await.unwrap();
}

/// ЗВУК ПО КНОПКЕ: без клика дорожек нет, с кликом — есть.
///
/// Механика веб-клиента Zoom, повторённая моком (`audiogate=1`): пока не
/// нажата «войти со звуком компьютера», в странице нет ни одного источника
/// звука, и тапы захвата сидят на пустом графе. Запись вышла бы нулевой длины
/// при полностью исправном боте — самая дорогая из возможных поломок, потому
/// что заметна только после встречи.
#[tokio::test]
async fn the_join_audio_button_is_what_makes_the_recording_exist() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url("per-track", "autojoin=0&audiogate=1&duration=14000");
    let bot = ChromeBot::new(mock_config(root.path(), &url));

    let mut streams = bot.start(JoinMode::Normal).await.unwrap();
    let mut events = Vec::new();
    let mut frames = 0usize;
    collect(&mut streams, Duration::from_secs(10), &mut events, &mut frames).await;

    let kinds: Vec<&str> = events.iter().map(MeetEvent::kind).collect();
    eprintln!("[звук по кнопке] события: {kinds:?}, кадров {frames}");

    // 1. Страница подтверждает, что кнопку нажали ИМЕННО МЫ.
    let clicks = ask_page(&bot, "JSON.stringify(window.__mockMeeting.clicks)")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(clicks.contains("join-audio"), "кнопка звука не нажата: {clicks}");

    // 2. И звук ПОШЁЛ — а это и есть то, ради чего клик существует.
    assert!(frames > 0, "после подключения звука не приехало ни кадра");
    assert!(kinds.contains(&"recording_started"), "{kinds:?}");

    bot.stop(StopReason::UserStop).await.unwrap();
}

/// ГОРЯЧИЕ КЛАВИШИ гасят устройства там, где кнопки не работают.
///
/// Мок с `hotkeys=1` принимает только `Ctrl+D` и `Ctrl+E`, а клики по кнопкам
/// игнорирует — так проверяется, что микрофон погашен ИМЕННО СОЧЕТАНИЕМ.
/// Проверять это фейковым DOM бессмысленно: `Input.dispatchKeyEvent` — это
/// команда браузеру, и работает она либо в браузере, либо нигде.
#[tokio::test]
async fn the_device_hotkeys_reach_a_real_page() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url("per-track", "autojoin=0&hotkeys=1&duration=14000");
    let bot = ChromeBot::new(mock_config(root.path(), &url));

    let mut streams = bot.start(JoinMode::Normal).await.unwrap();
    let mut events = Vec::new();
    let mut frames = 0usize;
    collect(&mut streams, Duration::from_secs(10), &mut events, &mut frames).await;

    let hotkeys = ask_page(&bot, "JSON.stringify(window.__mockMeeting.hotkeys)")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mic = ask_page(&bot, "window.__mockMeeting.micEnabled").await;
    let cam = ask_page(&bot, "window.__mockMeeting.camEnabled").await;
    eprintln!("[сочетания] {hotkeys}, mic={mic}, cam={cam}");

    assert!(hotkeys.contains("mic"), "микрофон не переключён сочетанием: {hotkeys}");
    assert!(hotkeys.contains("cam"), "камера не переключена сочетанием: {hotkeys}");
    assert_eq!(mic.as_bool(), Some(false), "микрофон остался включён");
    assert_eq!(cam.as_bool(), Some(false), "камера осталась включена");

    bot.stop(StopReason::UserStop).await.unwrap();
}
