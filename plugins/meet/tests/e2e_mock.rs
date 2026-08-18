//! Сквозной прогон на мок-встрече: настоящий Chrome → инжект → биндинг →
//! запись → файлы на диске → сверка с `expected.json`.
//!
//! За переменной `FORGE_MEET_E2E=1`: CI без браузера обязан оставаться
//! зелёным.
//!
//! ```bash
//! FORGE_MEET_E2E=1 cargo test -p forge-meet --test e2e_mock -- --nocapture
//! ```
//!
//! # Что именно проверяется
//!
//! То, чего не может проверить ни один герметичный тест: что инжект реально
//! доезжает до страницы, что тап ловит удалённые дорожки, что worklet считает
//! PCM и VAD в живом аудиопотоке, что кадры доходят через CDP-биндинг и что
//! таймлайн на выходе совпадает с расписанием страницы.
//!
//! # Герметичность
//!
//! Профиль Chrome, кэш плагина и каталог встречи — во временном каталоге.
//! После прогона живых процессов Chrome не остаётся: проверяется ЯВНО, по
//! `pid`, а не «наверное, закрылся».
//!
//! # Окружение без рендеринга Web Audio
//!
//! Обнаружено при отладке: бывают окружения, где Chrome стартует, страница
//! грузится, `AudioContext.state === "running"`, а `currentTime` ЗАМИРАЕТ на
//! первом же кванте (256/48000 = 0.00533 с) — аудиопоток не крутится вовсе.
//! В таком окружении мок-страница физически не может произвести звук, а
//! worklet — ни одного кадра, и тест проверял бы тишину, называя её успехом.
//!
//! Поэтому прогон НАЧИНАЕТСЯ с измерения аудиочасов и при замершем времени
//! ПРОПУСКАЕТСЯ с явным сообщением. Молча зелёный тест здесь был бы хуже
//! красного.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use forge_cdp::inject::BindingReceiver;
use forge_cdp::launch::{launch, ChromeProcess, LaunchConfig};
use forge_cdp::page::{CdpPage, PageSetup};
use forge_cdp::session::CdpBrowser;
use forge_cdp::transport::TransportConfig;
use forge_cdp::ws::WsTransport;
use forge_meet::bot::inject;
use forge_meet::bot::transport::{AudioTransport, CdpBindingTransport};
use forge_meet::config::MeetConfig;
use forge_meet::ingest::journal::EventJournal;
use forge_meet::ingest::{Manifest, Recorder};
use forge_meet::testing::{e2e_enabled, mock_meeting_dir, StaticServer};
use forge_meet::{detect_track_mode, TrackMode, TrackObservation};
use serde_json::Value;

/// Сколько ждать хвост кадров после конца встречи.
const DRAIN_TAIL: Duration = Duration::from_millis(1500);

/// Перевод режима окна из конфига плагина в режим запуска браузера.
///
/// Два перечисления с одинаковыми именами живут в разных крейтах намеренно:
/// `forge-cdp` ничего не знает о встречах, а плагин — о ключах командной
/// строки Chrome. Ф4 сделает этот же перевод в боевом коде; здесь он нужен,
/// чтобы e2e шёл в том режиме, который выбран дефолтом.
fn cdp_window_mode(mode: forge_meet::config::WindowMode) -> forge_cdp::launch::WindowMode {
    use forge_cdp::launch::WindowMode as Cdp;
    use forge_meet::config::WindowMode as Cfg;
    match mode {
        Cfg::Offscreen => Cdp::Offscreen,
        Cfg::Minimized => Cdp::Minimized,
        Cfg::Visible => Cdp::Visible,
        Cfg::Headless => Cdp::Headless,
    }
}

fn expected() -> Value {
    let raw = std::fs::read_to_string(mock_meeting_dir().join("expected.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// Интервал речи, снятый с записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval {
    start_ms: u64,
    end_ms: u64,
}

/// Итог одного прогона.
struct Run {
    meeting_dir: PathBuf,
    /// Каталог профиля ИМЕННО ЭТОГО прогона.
    ///
    /// Нужен проверке «после прогона не осталось живого Chrome»: искать в `ps`
    /// подстроку `chrome-profile` нельзя — под этим именем работают браузеры
    /// СОСЕДНИХ тестов, и проверка считала их своими остатками
    /// (`forge-8ecrh`).
    profile_dir: PathBuf,
    manifest: Manifest,
    /// Интервалы речи по номеру дорожки.
    intervals: BTreeMap<usize, Vec<Interval>>,
    frames: u64,
    /// Крутились ли аудиочасы страницы.
    audio_ran: bool,
}

/// Гасит Chrome при выходе ЛЮБЫМ путём — включая панику ассерта.
///
/// Без этого первый же упавший ассерт оставлял бы живой браузер с захваченным
/// микрофоном, и следующий прогон дрался бы с ним за профиль.
struct ChromeGuard(Option<ChromeProcess>);

impl ChromeGuard {
    fn pid(&self) -> i32 {
        self.0.as_ref().map(|c| c.pid).unwrap_or(0)
    }

    async fn terminate(&mut self) {
        if let Some(mut chrome) = self.0.take() {
            let _ = chrome.terminate(Duration::from_secs(5)).await;
        }
    }
}

impl Drop for ChromeGuard {
    fn drop(&mut self) {
        if let Some(chrome) = &self.0 {
            // Синхронный путь: аварийный, `terminate` уже не позвать.
            unsafe {
                libc::killpg(chrome.pid, libc::SIGKILL);
            }
        }
    }
}

/// Полный прогон мок-встречи в заданном режиме.
async fn run_mode(mode: &str, duration_ms: u64) -> Run {
    run_mode_scanning(mode, duration_ms, None).await
}

/// Прогон с заданным интервалом обхода приёмников.
///
/// Интервал вынесен в параметр не ради гибкости, а ради СТРАЖА. Дефект
/// «каждая дорожка захватывается дважды» жил в обходе приёмников и проявлялся
/// только на прогонах длиннее двух секунд: короткий тест успевал закончиться
/// раньше первого обхода и рапортовал верное число дорожек. Проверка, которая
/// ловит дефект лишь при удачной длительности, — не проверка. Поэтому тест
/// дубля СТАВИТ интервал в четверть секунды и получает несколько обходов на
/// заведомо коротком прогоне.
async fn run_mode_scanning(mode: &str, duration_ms: u64, receiver_scan_ms: Option<u64>) -> Run {
    // ⚠ ЖИВЫЕ ПРОГОНЫ ИДУТ ПО ОДНОМУ (`forge-8ecrh`). Пять браузеров разом
    // отнимают друг у друга процессор, и короткий прогон получает ноль кадров
    // на исправном коде. Замок держится, пока жив Chrome этого теста.
    let _browser = forge_meet::testing::browser_guard();
    let root = tempfile::tempdir().expect("временный каталог");
    let meeting_dir = root.path().join("meeting");
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url(mode, &format!("duration={duration_ms}"));

    let cfg = MeetConfig {
        config_path: root.path().join("meet.toml"),
        cache_dir: root.path().join("cache"),
        db_path: root.path().join("cache/meet.db"),
        models_dir: root.path().join("cache/models"),
        ..MeetConfig::default()
    };

    let mut launch_cfg = LaunchConfig::new(&meeting_dir, root.path());
    // Прогон идёт В ДЕФОЛТНОМ РЕЖИМЕ ОКНА, а не в удобном для теста. Дефолт
    // `minimized` выбран измерением (`tests/window_mode.rs`), и держать e2e на
    // другом режиме означало бы проверять не то, что работает у пользователя.
    // Заодно окно не мешает работать во время прогона.
    launch_cfg.window_mode = cdp_window_mode(MeetConfig::default().window_mode);
    let chrome = launch(&launch_cfg).await.expect("Chrome не запустился");
    let mut guard = ChromeGuard(Some(chrome));
    let ws_url = guard.0.as_ref().unwrap().ws_url.clone();

    let transport = WsTransport::connect(&ws_url, TransportConfig::default())
        .await
        .expect("нет соединения с CDP");
    let browser = CdpBrowser::new(transport);
    let mut page = CdpPage::open(&browser).await.expect("вкладка не открылась");

    let inject_cfg = match receiver_scan_ms {
        None => inject::inject_config(&cfg),
        Some(ms) => {
            let mut params = inject::InjectParams::from_config(&cfg);
            params.receiver_scan_ms = ms;
            forge_cdp::InjectConfig::with_script(inject::page_source(&params))
        }
    };
    let binding_name = inject_cfg.binding_name.clone();
    // ПОДПИСКА ДО НАВИГАЦИИ. Первые кадры приходят сразу после загрузки, и
    // окно между `navigate` и `subscribe` — это потерянное начало записи.
    let rx = BindingReceiver::new(page.session().subscribe(), binding_name.clone());

    let setup = PageSetup {
        inject: inject_cfg,
        window_mode: launch_cfg.window_mode,
        ..PageSetup::default()
    };
    page.prepare_and_navigate(&url, &setup)
        .await
        .expect("навигация не удалась");

    // Аудиочасы страницы: если они стоят, звука не будет ни у кого.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let audio_ran = audio_clock_advances(&page).await;
    let page_t0_unix = page_t0_unix_ms(&page).await;

    let manifest = Manifest::new(
        "e2e",
        &url,
        "telemost",
        "normal",
        cfg.audio_sample_rate,
    );
    let mut recorder = Recorder::create(&meeting_dir, manifest).expect("каталог встречи");
    let mut cdp_transport = CdpBindingTransport::new(rx, cfg.audio_sample_rate);

    let deadline = tokio::time::Instant::now() + Duration::from_millis(duration_ms) + DRAIN_TAIL;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, cdp_transport.recv()).await {
            Ok(Some(msg)) => {
                recorder.accept(msg).expect("кадр не записался");
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let stats = cdp_transport.stats();
    let report = recorder.finish().expect("запись не закрылась");

    guard.terminate().await;
    let pid = guard.pid();
    assert!(
        !forge_cdp::orphans::is_alive(pid),
        "после прогона остался живой Chrome (pid {pid})"
    );

    let manifest = Manifest::load(&meeting_dir).expect("манифест не прочитался");
    // ДВЕ ОСИ СВОДЯТСЯ ЗДЕСЬ. Эталон живёт на оси расписания страницы, запись
    // — на оси первой захваченной дорожки. Между ними пара сотен
    // миллисекунд, и на загруженной машине разрыв растёт; без вычитания
    // допуск в 250 мс тратился бы на эту разницу, а не на точность детекта.
    let axis_shift = page_t0_unix
        .and_then(|page_t0| {
            let ours = manifest.t0_unix_ms;
            (ours > 0 && page_t0 >= ours).then_some((page_t0 - ours) as u64)
        })
        .unwrap_or(0);
    let intervals = intervals_from_journal(&meeting_dir, &manifest, axis_shift);

    let paths: Vec<&str> = manifest
        .tracks
        .iter()
        .map(|t| t.capture_path.as_deref().unwrap_or("?"))
        .collect();
    eprintln!(
        "[{mode}] кадров {}, сэмплов {}, дорожек {} {paths:?}, путь {}, длительность {} мс, \
         сдвиг осей {axis_shift} мс, аудиочасы {}",
        stats.frames,
        stats.samples,
        report.tracks,
        manifest.capture_path.as_deref().unwrap_or("—"),
        report.duration_ms,
        if audio_ran { "идут" } else { "СТОЯТ" }
    );

    // `root` держим до конца: TempDir удаляет каталог при drop, а мы только
    // что прочитали из него манифест.
    let run = Run {
        meeting_dir: meeting_dir.clone(),
        profile_dir: launch_cfg.user_data_dir.clone(),
        manifest,
        intervals,
        frames: report.frames,
        audio_ran,
    };
    std::mem::forget(root);
    run
}

/// Идут ли аудиочасы страницы.
async fn audio_clock_advances(page: &CdpPage) -> bool {
    let expr = r#"JSON.stringify(window.__mockMeeting && window.__mockMeeting.audioClock
        ? window.__mockMeeting.audioClock() : null)"#;
    async fn read_clock(page: &CdpPage, expr: &str) -> Option<f64> {
        page.session()
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await
            .ok()
            .and_then(|v| v["result"]["value"].as_str().map(str::to_string))
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v["currentTime"].as_f64())
    }
    let first = read_clock(page, expr).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let second = read_clock(page, expr).await;
    match (first, second) {
        (Some(a), Some(b)) => b > a + 0.05,
        _ => false,
    }
}

/// Unix-момент начала расписания страницы. `None` — страница его не отдала.
async fn page_t0_unix_ms(page: &CdpPage) -> Option<i64> {
    let expr = "window.__mockMeeting ? (window.__mockMeeting.t0UnixMs || 0) : 0";
    page.session()
        .call(
            "Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true}),
        )
        .await
        .ok()
        .and_then(|v| v["result"]["value"].as_i64())
        .filter(|v| *v > 0)
}

/// Собирает интервалы речи по журналу записи.
///
/// `axis_shift` — сколько миллисекунд ось записи опережает ось расписания
/// страницы. Вычитается сразу, поэтому дальше по тесту все времена живут в
/// одной системе координат — той, в которой написан эталон.
fn intervals_from_journal(
    dir: &std::path::Path,
    manifest: &Manifest,
    axis_shift: u64,
) -> BTreeMap<usize, Vec<Interval>> {
    let by_track: BTreeMap<String, usize> = manifest
        .tracks
        .iter()
        .map(|t| (t.track_id.clone(), t.idx))
        .collect();

    let mut open: BTreeMap<usize, u64> = BTreeMap::new();
    let mut out: BTreeMap<usize, Vec<Interval>> = BTreeMap::new();
    for entry in EventJournal::read(dir).unwrap_or_default() {
        let track = entry.data.get("t").and_then(Value::as_str);
        let Some(idx) = track.and_then(|t| by_track.get(t)).copied() else {
            continue;
        };
        let at = entry.data.get("at").and_then(Value::as_u64).unwrap_or(entry.at_ms);
        match entry.kind.as_str() {
            "speech_start" => {
                open.entry(idx).or_insert(at);
            }
            "speech_end" => {
                if let Some(start) = open.remove(&idx) {
                    if at > start {
                        out.entry(idx).or_default().push(Interval {
                            start_ms: start,
                            end_ms: at,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    // Незакрытые интервалы закрываем длительностью записи: последняя реплика
    // встречи, оборванная выходом бота, тоже реплика.
    for (idx, start) in open {
        if manifest.duration_ms > start {
            out.entry(idx).or_default().push(Interval {
                start_ms: start,
                end_ms: manifest.duration_ms,
            });
        }
    }
    for ivs in out.values_mut() {
        for iv in ivs.iter_mut() {
            iv.start_ms = iv.start_ms.saturating_sub(axis_shift);
            iv.end_ms = iv.end_ms.saturating_sub(axis_shift);
        }
    }
    out
}

/// Говорит ли хоть одна дорожка в момент `t`.
fn speakers_at(intervals: &BTreeMap<usize, Vec<Interval>>, t: u64) -> Vec<usize> {
    intervals
        .iter()
        .filter(|(_, ivs)| ivs.iter().any(|iv| t >= iv.start_ms && t < iv.end_ms))
        .map(|(idx, _)| *idx)
        .collect()
}

/// Общая проверка режима.
async fn check_mode(mode: &str) {
    let exp = expected();
    let spec = &exp["modes"][mode];
    let duration = exp["duration_ms"].as_u64().unwrap();
    let tol_start = exp["tolerance_ms"]["speech_start"].as_u64().unwrap();

    let run = run_mode(mode, duration).await;

    if !run.audio_ran {
        eprintln!(
            "ПРОПУСК [{mode}]: окружение не крутит аудиопоток Chrome \
             (AudioContext.currentTime замирает на первом кванте). \
             Мок-страница физически не может произвести звук, проверять нечего."
        );
        return;
    }

    // 1. Кадры дошли.
    assert!(run.frames > 0, "[{mode}] через биндинг не пришло ни одного кадра");

    // 2. Число дорожек соответствует режиму.
    let want_tracks = spec["audio_tracks"].as_u64().unwrap() as usize;
    assert_eq!(
        run.manifest.tracks.len(),
        want_tracks,
        "[{mode}] дорожек {}, ожидалось {want_tracks}",
        run.manifest.tracks.len()
    );

    // 3. Файлы на месте и не пусты.
    for t in &run.manifest.tracks {
        let path = run.meeting_dir.join("raw").join(t.file_name());
        assert!(path.is_file(), "[{mode}] нет {}", path.display());
        let samples = forge_meet::ingest::wav::read_samples(&path).unwrap();
        assert!(
            samples.iter().any(|&s| s != 0),
            "[{mode}] дорожка {} — сплошная тишина",
            t.idx
        );
    }

    // 4. Путь захвата — тот, что ожидается.
    assert_eq!(
        run.manifest.capture_path.as_deref(),
        spec["capture_path"].as_str(),
        "[{mode}] сработал не тот тап"
    );

    // 5. Режим SFU определяется правильно по фактическим дорожкам.
    let obs = TrackObservation {
        audio_tracks: run.manifest.tracks.len(),
        participants: spec["participants_in_dom"].as_u64().unwrap() as usize,
        mid_stable: true,
        multi_csrc: false,
        since_join_ms: 60_000,
    };
    let want_mode = TrackMode::parse(spec["track_mode"].as_str().unwrap()).unwrap();
    assert_eq!(
        detect_track_mode(&obs, 0),
        want_mode,
        "[{mode}] режим SFU определён неверно"
    );

    // 6. Интервалы речи совпадают с эталоном.
    let mut got: Vec<Vec<Interval>> = run.intervals.values().cloned().collect();
    got.sort_by_key(|ivs| ivs.first().map(|i| i.start_ms).unwrap_or(u64::MAX));
    let mut want: Vec<Vec<(u64, u64)>> = spec["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            t["speech"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    (
                        s["start_ms"].as_u64().unwrap(),
                        s["end_ms"].as_u64().unwrap(),
                    )
                })
                .collect()
        })
        .collect();
    want.sort_by_key(|ivs| ivs.first().map(|i| i.0).unwrap_or(u64::MAX));

    assert_eq!(
        got.len(),
        want.len(),
        "[{mode}] дорожек с речью {}, ожидалось {}: {got:?}",
        got.len(),
        want.len()
    );
    for (n, (got_track, want_track)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(
            got_track.len(),
            want_track.len(),
            "[{mode}] дорожка {n}: интервалов {}, ожидалось {} — {got_track:?} против {want_track:?}",
            got_track.len(),
            want_track.len()
        );
        for (k, (iv, w)) in got_track.iter().zip(want_track.iter()).enumerate() {
            assert!(
                iv.start_ms.abs_diff(w.0) <= tol_start,
                "[{mode}] дорожка {n}, интервал {k}: начало {} против эталонных {} (допуск {tol_start})",
                iv.start_ms,
                w.0
            );
        }
    }

    // 7. НАЛОЖЕНИЕ РЕЧИ распознано как наложение, а не как один говорящий.
    // Ради этого пер-трековый захват и делается.
    if want_tracks > 1 {
        for overlap in spec["overlaps"].as_array().unwrap() {
            let (from, to) = (
                overlap["start_ms"].as_u64().unwrap(),
                overlap["end_ms"].as_u64().unwrap(),
            );
            let mid = (from + to) / 2;
            let speaking = speakers_at(&run.intervals, mid);
            assert!(
                speaking.len() >= 2,
                "[{mode}] на {mid} мс обязано говорить двое, а говорит {speaking:?}"
            );
        }
    }

    // 8. В заявленной тишине никто не говорит: завышенный порог VAD ловится
    // именно этим.
    for silence in spec["silence"].as_array().unwrap() {
        let (from, to) = (
            silence["start_ms"].as_u64().unwrap(),
            silence["end_ms"].as_u64().unwrap(),
        );
        // Берём середину участка: края закономерно размыты hangover'ом.
        let mid = (from + to) / 2;
        let speaking = speakers_at(&run.intervals, mid);
        assert!(
            speaking.is_empty(),
            "[{mode}] на {mid} мс заявлена тишина, а говорят {speaking:?}"
        );
    }
}

#[tokio::test]
async fn per_track_mode_records_every_speaker_separately() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    check_mode("per-track").await;
}

#[tokio::test]
async fn slots_mode_records_the_fixed_set_of_tracks() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    check_mode("slots").await;
}

#[tokio::test]
async fn mixed_mode_records_the_single_mixed_track() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    check_mode("mixed").await;
}

/// ОДИН ИСТОЧНИК — ОДНА ДОРОЖКА, и это видно на КОРОТКОМ прогоне.
///
/// История проверки важнее её текста. Живой прогон однажды дал ровно вдвое
/// больше дорожек, чем источников, во всех трёх режимах — обход `getReceivers`
/// брал приёмники и у отправляющей стороны, где на каждый `addTrack` заведён
/// трансивер с вечно молчащим приёмным треком. Дефект НЕ ловился коротким
/// тестом: тот успевал закончиться раньше первого обхода. Поэтому здесь обход
/// форсирован интервалом в четверть секунды, а прогон нарочно короткий — так
/// страж срабатывает при каждом запуске, а не при удачной длительности.
///
/// Проверка идёт и в окружении, где Chrome не крутит Web Audio: состав
/// дорожек берётся из событий WebRTC и от звука не зависит вовсе.
#[tokio::test]
async fn every_source_gives_exactly_one_track() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    for (mode, want) in [("per-track", 3usize), ("slots", 2), ("mixed", 1)] {
        let run = run_mode_scanning(mode, 2_500, Some(250)).await;
        let ids: Vec<&str> = run
            .manifest
            .tracks
            .iter()
            .map(|t| t.track_id.as_str())
            .collect();
        assert_eq!(
            run.manifest.tracks.len(),
            want,
            "[{mode}] дорожек {}, источников {want}: тапы снова складываются вместо приоритета — {ids:?}",
            run.manifest.tracks.len()
        );
        // Путь пишется ПОДОРОЖЕЧНО: без него, разбирая запись, нельзя
        // отличить настоящую дорожку от зеркала Web Audio.
        for t in &run.manifest.tracks {
            assert_eq!(
                t.capture_path.as_deref(),
                Some("rtcpeerconnection"),
                "[{mode}] дорожка {} взята не тем тапом",
                t.idx
            );
        }
        assert_eq!(
            run.manifest.capture_path.as_deref(),
            Some("rtcpeerconnection"),
            "[{mode}] общий путь захвата не совпал с фактическим"
        );
    }
}

/// Профиль, кэш и база — во временном каталоге, а после прогона не остаётся ни
/// одного живого Chrome. Проверяется по `pid`, а не «наверное, закрылся».
#[tokio::test]
async fn the_run_is_hermetic_and_leaves_no_chrome_behind() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let run = run_mode("per-track", 1_500).await;
    assert_eq!(
        run.manifest.tracks.len(),
        3,
        "короткий прогон обязан давать столько же дорожек, сколько источников"
    );
    assert!(
        run.meeting_dir.starts_with(std::env::temp_dir())
            || run.meeting_dir.to_string_lossy().contains("/var/folders/"),
        "каталог встречи вне временного: {}",
        run.meeting_dir.display()
    );
    // `run_mode` уже проверил отсутствие живого процесса по pid; здесь
    // страхуемся от остатков С НАШИМ ПРОФИЛЕМ.
    //
    // ⚠ Фильтр по ПОЛНОМУ пути профиля, а не по подстроке `chrome-profile`:
    // под этим именем работают браузеры соседних тестов файла, и прежняя
    // редакция считала их своими остатками (`forge-8ecrh`). Полный путь
    // уникален — он лежит внутри `TempDir` этого прогона.
    let profile = run.profile_dir.display().to_string();
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
    assert_eq!(leftovers, 0, "в системе остались Chrome с профилем {profile}");
}
