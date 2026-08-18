//! Мок-встреча как тестовый актив: согласованность страницы и эталона.
//!
//! Эти проверки ГЕРМЕТИЧНЫ — ни браузера, ни сети. Они существуют потому, что
//! расписание речи дублируется в двух местах: в `index.html` (там оно
//! исполняется) и в `expected.json` (там оно проверяется). Дублирование
//! намеренное — эталон, вычисленный тем же кодом, который проверяется, не
//! проверяет ничего, — но разъехаться эти два файла не имеют права: e2e тогда
//! молча перестал бы что-либо ловить.
//!
//! Живой прогон страницы в Chrome — в `tests/e2e_mock.rs` за `FORGE_MEET_E2E=1`.

use forge_meet::testing::{e2e_enabled, mock_meeting_dir, StaticServer};
use serde_json::Value;

fn expected() -> Value {
    let raw = std::fs::read_to_string(mock_meeting_dir().join("expected.json"))
        .expect("expected.json обязан лежать рядом со страницей");
    serde_json::from_str(&raw).expect("expected.json не разбирается")
}

fn page() -> String {
    std::fs::read_to_string(mock_meeting_dir().join("index.html")).expect("index.html не читается")
}

#[test]
fn the_fixture_has_all_three_files() {
    let dir = mock_meeting_dir();
    for name in ["index.html", "expected.json", "README.md"] {
        assert!(dir.join(name).is_file(), "нет {name} в {}", dir.display());
    }
}

/// Три режима SFU, и они РАЗНЫЕ по числу дорожек. Совпади они — тест «режим
/// определён правильно» проходил бы при любой реализации детекта.
#[test]
fn the_three_modes_differ_in_track_count() {
    let exp = expected();
    let modes = exp["modes"].as_object().expect("нет секции modes");
    assert_eq!(modes.len(), 3, "режимов обязано быть три: {:?}", modes.keys());

    let mut counts = Vec::new();
    for (name, m) in modes {
        let tracks = m["audio_tracks"].as_u64().expect("нет audio_tracks");
        let listed = m["tracks"].as_array().expect("нет списка дорожек").len() as u64;
        assert_eq!(tracks, listed, "{name}: audio_tracks не совпадает с длиной tracks");
        counts.push(tracks);
    }
    counts.sort_unstable();
    counts.dedup();
    assert_eq!(counts.len(), 3, "режимы дают одинаковое число дорожек");

    assert_eq!(exp["modes"]["per-track"]["track_mode"], "per_participant");
    assert_eq!(exp["modes"]["slots"]["track_mode"], "slots");
    assert_eq!(exp["modes"]["mixed"]["track_mode"], "mixed");
}

/// Режим, объявленный в эталоне, обязан совпасть с тем, что скажет
/// `detect_track_mode` по числу дорожек и участников. Иначе e2e сверял бы
/// эталон сам с собой.
#[test]
fn the_expected_mode_matches_what_the_detector_would_say() {
    use forge_meet::{detect_track_mode, TrackMode, TrackObservation};

    let exp = expected();
    for (name, m) in exp["modes"].as_object().unwrap() {
        let obs = TrackObservation {
            audio_tracks: m["audio_tracks"].as_u64().unwrap() as usize,
            participants: m["participants_in_dom"].as_u64().unwrap() as usize,
            mid_stable: true,
            // Мок не эмулирует CSRC: микс определяется соотношением «одна
            // дорожка на троих», и это ровно тот путь, который надо проверить.
            multi_csrc: false,
            since_join_ms: 60_000,
        };
        let want = TrackMode::parse(m["track_mode"].as_str().unwrap()).unwrap();
        assert_eq!(
            detect_track_mode(&obs, 0),
            want,
            "режим {name}: {} дорожек на {} участников",
            obs.audio_tracks,
            obs.participants
        );
    }
}

/// **Наложение речи обязано быть в каждом режиме.** Ради него пер-трековый
/// захват и делается: в миксе перебивание неразделимо, по дорожкам —
/// разбирается тривиально. Режим без наложения не проверял бы главного.
#[test]
fn every_mode_has_deliberate_overlapping_speech() {
    let exp = expected();
    for (name, m) in exp["modes"].as_object().unwrap() {
        let overlaps = m["overlaps"].as_array().unwrap_or_else(|| panic!("{name}: нет overlaps"));
        assert!(!overlaps.is_empty(), "{name}: наложение речи не запланировано");
        for o in overlaps {
            let (s, e) = (o["start_ms"].as_u64().unwrap(), o["end_ms"].as_u64().unwrap());
            assert!(e > s, "{name}: пустой участок наложения {s}..{e}");
            assert!(
                o["speakers"].as_array().unwrap().len() >= 2,
                "{name}: «наложение» с одним говорящим — это не наложение"
            );
        }
    }
}

/// Каждый интервал речи внутри дорожки: непустой, в пределах встречи и не
/// пересекается сам с собой (один человек не перебивает сам себя).
#[test]
fn the_intervals_inside_one_track_are_sane() {
    let exp = expected();
    let duration = exp["duration_ms"].as_u64().unwrap();
    for (name, m) in exp["modes"].as_object().unwrap() {
        for track in m["tracks"].as_array().unwrap() {
            let idx = track["index"].as_u64().unwrap();
            let speech = track["speech"].as_array().unwrap();
            assert!(!speech.is_empty(), "{name}/{idx}: дорожка молчит всю встречу");
            let mut prev_end = 0u64;
            for sp in speech {
                let (s, e) = (sp["start_ms"].as_u64().unwrap(), sp["end_ms"].as_u64().unwrap());
                assert!(e > s, "{name}/{idx}: интервал {s}..{e} пуст");
                assert!(e <= duration, "{name}/{idx}: интервал {s}..{e} за пределами встречи");
                // Поле `speech` — это то, что дорожка отдаёт НАБЛЮДАТЕЛЮ,
                // поэтому пересечений в нём не бывает даже у микса: там
                // источники слиты, а исходные интервалы лежат в `sources`.
                assert!(
                    s >= prev_end,
                    "{name}/{idx}: интервалы одной дорожки пересекаются ({prev_end} → {s})"
                );
                prev_end = e;
            }
        }
    }
}

/// Участки тишины: их проверяют отдельно, потому что завышенный порог VAD
/// ловится именно «речью там, где её нет».
#[test]
fn every_mode_declares_silent_stretches() {
    let exp = expected();
    for (name, m) in exp["modes"].as_object().unwrap() {
        let silence = m["silence"].as_array().unwrap_or_else(|| panic!("{name}: нет silence"));
        assert!(!silence.is_empty(), "{name}: тишины не заявлено");
        for s in silence {
            let (from, to) = (s["start_ms"].as_u64().unwrap(), s["end_ms"].as_u64().unwrap());
            assert!(to > from, "{name}: пустой участок тишины");
            // И в этот участок не имеет права попасть ни один интервал речи.
            for track in m["tracks"].as_array().unwrap() {
                for sp in track["speech"].as_array().unwrap() {
                    let (ss, se) = (sp["start_ms"].as_u64().unwrap(), sp["end_ms"].as_u64().unwrap());
                    assert!(
                        se <= from || ss >= to,
                        "{name}: заявленная тишина {from}..{to} пересекается с речью {ss}..{se}"
                    );
                }
            }
        }
    }
}

/// **Расписание страницы и эталон обязаны совпадать.** Это и есть та проверка,
/// ради которой файл существует: правка `SCHEDULES` без правки `expected.json`
/// не должна проходить молча.
#[test]
fn the_page_schedule_matches_the_expected_timeline() {
    let src = page();
    let exp = expected();

    for (name, m) in exp["modes"].as_object().unwrap() {
        for track in m["tracks"].as_array().unwrap() {
            // У микса расписание страницы — это `sources` (кто подмешан), а
            // `speech` уже слито наблюдателем. Сверять со страницей надо
            // исходное.
            let scheduled = track
                .get("sources")
                .or_else(|| track.get("speech"))
                .and_then(Value::as_array)
                .unwrap();
            for sp in scheduled {
                let (s, e) = (sp["start_ms"].as_u64().unwrap(), sp["end_ms"].as_u64().unwrap());
                // Форма записи в странице: `from: 500,  to: 2000`.
                let needle_from = format!("from: {s},");
                let needle_to = format!("to: {e}");
                assert!(
                    src.contains(&needle_from),
                    "{name}: в index.html нет начала интервала {s} мс"
                );
                assert!(
                    src.contains(&needle_to),
                    "{name}: в index.html нет конца интервала {e} мс"
                );
            }
        }
        // Тоны дорожек тоже часть контракта: по частоте видно, чей файл
        // открыт, и e2e отличает дорожки именно по ней.
        for track in m["tracks"].as_array().unwrap() {
            let mut tones = vec![track["tone_hz"].as_u64().unwrap()];
            if let Some(sources) = track.get("sources").and_then(Value::as_array) {
                tones.extend(sources.iter().map(|s| s["tone_hz"].as_u64().unwrap()));
            }
            for hz in tones {
                assert!(
                    src.contains(&format!("hz: {hz}")),
                    "{name}: в index.html нет тона {hz} Гц"
                );
            }
        }
    }

    // Длительность и частота дискретизации страницы — тоже часть контракта.
    let duration = exp["duration_ms"].as_u64().unwrap();
    assert!(
        src.contains(&format!("\"{duration}\"")),
        "длительность {duration} не совпадает с index.html"
    );
    let rate = exp["sample_rate_page"].as_u64().unwrap();
    assert!(
        src.contains(&format!("sampleRate: {rate}")),
        "частота страницы {rate} не совпадает с index.html"
    );
}

/// Селекторы из эталона обязаны реально присутствовать на странице. Ф4
/// перенесёт их в `meet.probes.toml`, и разъезд означал бы бота, который не
/// видит участников на собственном стенде.
#[test]
fn every_documented_selector_exists_on_the_page() {
    let src = page();
    let exp = expected();
    let dom = exp["dom"].as_object().expect("нет секции dom");
    assert!(dom.len() >= 12, "описано подозрительно мало селекторов: {}", dom.len());

    for (key, sel) in dom {
        // `speaking_attr` — не `data-testid`, он проверяется отдельно ниже.
        if key.starts_with('_') || key == "speaking_attr" {
            continue;
        }
        let sel = sel.as_str().unwrap();
        // Из `[data-testid="participant"]` берём само значение: сравнивать
        // селектор целиком нельзя, в странице он записан атрибутом.
        let needle = sel
            .split_once("data-testid=\"")
            .map(|(_, rest)| rest.split('"').next().unwrap_or("").to_string())
            .unwrap_or_else(|| sel.to_string());
        if needle.is_empty() {
            continue;
        }
        // Две формы записи: разметкой (`data-testid="participant-list"`) и
        // кодом (`setAttribute("data-testid", "participant")`) — список
        // участников строится динамически, как на живой площадке.
        let in_markup = src.contains(&format!("data-testid=\"{needle}\""));
        let in_code = src.contains(&format!("\"data-testid\", \"{needle}\""));
        assert!(
            in_markup || in_code,
            "селектор {key} = {sel} на странице отсутствует"
        );
    }

    // Атрибут говорящего проверяется отдельно: он не `data-testid`.
    let attr = dom["speaking_attr"].as_str().unwrap();
    assert!(src.contains(attr), "атрибут активного спикера {attr} на странице отсутствует");
}

/// Страница не имеет права зависеть от внешних ресурсов: стенд обязан
/// работать без сети, иначе он однажды упадёт не из-за нашего кода.
#[test]
fn the_page_is_self_contained() {
    let src = page();
    for forbidden in ["http://", "https://", "//cdn", "<script src"] {
        assert!(
            !src.contains(forbidden),
            "страница тянет что-то извне ({forbidden}) — стенд обязан работать без сети"
        );
    }
}

/// Расписание задаётся АУДИОЧАСАМИ, а не `setTimeout`: окно бота occluded, и
/// Chrome троттлит таймеры рендерера до секунды. Расписание на таймерах
/// разъехалось бы на этом первым, и «эталонный» таймлайн стал бы случайным.
#[test]
fn the_schedule_runs_on_the_audio_clock_not_on_timers() {
    let src = page();
    assert!(
        src.contains("setValueAtTime(0.8, t0 + g.from / 1000)"),
        "гейты обязаны ставиться на абсолютную ось AudioContext.currentTime"
    );
    assert!(
        src.contains("(ctx.currentTime - t0)"),
        "DOM обязан обновляться ОТ аудиочасов, а не счётчиком тиков"
    );
    // setInterval в странице ровно один — тот, что ОПРАШИВАЕТ аудиочасы.
    assert_eq!(
        src.matches("setInterval(").count(),
        1,
        "второй таймер в странице означает второе, нетроттлингоустойчивое расписание"
    );
    assert_eq!(
        src.matches("setTimeout(").count(),
        0,
        "setTimeout в расписании мок-встречи запрещён"
    );
}

/// `window.__mockMeeting` — только для тестов. Проверка, что страница
/// объявляет его ровно один раз и явно об этом предупреждает: тап, опершийся
/// на такое поле, прошёл бы мок и провалился бы на живой площадке.
#[test]
fn the_test_only_api_is_declared_once_and_documented() {
    let src = page();
    assert_eq!(src.matches("window.__mockMeeting = mock").count(), 1);
    assert!(
        src.contains("Только для ТЕСТОВ"),
        "предупреждение о тестовом API обязано стоять рядом с ним"
    );
    let readme = std::fs::read_to_string(mock_meeting_dir().join("README.md")).unwrap();
    assert!(readme.contains("__mockMeeting"), "README обязан предупредить о тестовом API");
    assert!(readme.contains("FORGE_MEET_E2E=1"), "README обязан показать, как прогнать e2e");
}

// ---------------------------------------------------------------------------
// Живая проверка страницы в Chrome. За `FORGE_MEET_E2E=1`.
// ---------------------------------------------------------------------------
//
// Полный сквозной прогон пайплайна — в `tests/e2e_mock.rs`. Здесь проверяется
// сама СТРАНИЦА: что петлевое соединение поднимается, что три режима дают
// разное число удалённых дорожек и что расписание воспроизводится одинаково
// при повторе. Без этой проверки всё, что построено на моке, стояло бы на
// непроверенном фундаменте.

/// Открывает мок-встречу в своём Chrome и возвращает состояние страницы.
///
/// Профиль во временном каталоге, окно за экраном, процесс гасится в любом
/// случае: живой Chrome с захваченным микрофоном после теста — это уже не
/// тест, а последствия.
async fn probe_page(mode: &str, duration_ms: u64, wait_ms: u64) -> Value {
    use forge_cdp::launch::{launch, LaunchConfig};
    use forge_cdp::page::{CdpPage, PageSetup};
    use forge_cdp::session::CdpBrowser;
    use forge_cdp::transport::TransportConfig;
    use forge_cdp::ws::WsTransport;

    let dir = tempfile::tempdir().expect("временный каталог");
    let server = StaticServer::start(mock_meeting_dir()).expect("статический сервер");
    let url = server.mock_meeting_url(mode, &format!("duration={duration_ms}"));

    let cfg = LaunchConfig::new(dir.path().join("meeting"), dir.path());
    let mut chrome = launch(&cfg).await.expect("Chrome не запустился");
    let pid = chrome.pid;

    let result = async {
        let transport = WsTransport::connect(&chrome.ws_url, TransportConfig::default())
            .await
            .expect("нет соединения с CDP");
        let browser = CdpBrowser::new(transport);
        let mut page = CdpPage::open(&browser).await.expect("вкладка не открылась");
        page.prepare_and_navigate(&url, &PageSetup::default())
            .await
            .expect("навигация не удалась");

        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;

        let expr = r#"JSON.stringify({
            mode: window.__mockMeeting ? window.__mockMeeting.mode : null,
            joined: window.__mockMeeting ? window.__mockMeeting.joined : false,
            ended: window.__mockMeeting ? window.__mockMeeting.ended : false,
            remoteTracks: window.__mockMeeting ? window.__mockMeeting.remoteTracks.length : -1,
            participants: document.querySelectorAll('[data-testid="participant"]').length,
            endedMarker: !!document.querySelector('[data-testid="meeting-ended"][data-ended="true"]'),
            trackCountLabel: (document.querySelector('[data-testid="track-count"]') || {}).textContent || ""
        })"#;
        let res = page
            .session()
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await
            .expect("Runtime.evaluate не отработал");
        let raw = res["result"]["value"].as_str().unwrap_or("{}").to_string();
        serde_json::from_str::<Value>(&raw).expect("состояние страницы не разбирается")
    }
    .await;

    chrome
        .terminate(std::time::Duration::from_secs(3))
        .await
        .expect("Chrome не погасился");
    assert!(
        !forge_cdp::orphans::is_alive(pid),
        "после теста остался живой Chrome (pid {pid})"
    );
    result
}

/// **Три режима дают РАЗНОЕ число удалённых дорожек.** Совпади они — проверка
/// детекта режима не проверяла бы ничего.
#[tokio::test]
async fn the_page_gives_a_different_number_of_remote_tracks_per_mode() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let exp = expected();
    let mut seen = Vec::new();
    for mode in ["per-track", "slots", "mixed"] {
        let state = probe_page(mode, 4_000, 1_200).await;
        let want_tracks = exp["modes"][mode]["audio_tracks"].as_u64().unwrap();
        let want_people = exp["modes"][mode]["participants_in_dom"].as_u64().unwrap();

        assert_eq!(state["mode"].as_str(), Some(mode), "{state}");
        assert_eq!(state["joined"], true, "страница не вошла во встречу: {state}");
        assert_eq!(
            state["remoteTracks"].as_u64(),
            Some(want_tracks),
            "{mode}: число удалённых дорожек разошлось с эталоном: {state}"
        );
        assert_eq!(
            state["participants"].as_u64(),
            Some(want_people),
            "{mode}: число участников в DOM разошлось с эталоном: {state}"
        );
        seen.push(want_tracks);
    }
    seen.dedup();
    assert_eq!(seen.len(), 3, "режимы обязаны отличаться числом дорожек");
}

/// Повтор даёт то же самое: расписание висит на аудиочасах и не зависит ни от
/// планировщика, ни от нагрузки машины.
#[tokio::test]
async fn two_runs_of_the_same_mode_agree() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let a = probe_page("per-track", 4_000, 1_200).await;
    let b = probe_page("per-track", 4_000, 1_200).await;
    assert_eq!(a["remoteTracks"], b["remoteTracks"], "{a} против {b}");
    assert_eq!(a["participants"], b["participants"]);
    assert_eq!(a["trackCountLabel"], b["trackCountLabel"]);
}

/// Конец встречи виден ТРЕМЯ независимыми сигналами — ровно теми, на которые
/// опирается автовыход бота.
#[tokio::test]
async fn the_meeting_ends_by_itself_and_says_so() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    // Встреча на секунду, ждём две: маркер обязан появиться сам.
    let state = probe_page("per-track", 1_000, 2_200).await;
    assert_eq!(state["ended"], true, "страница не завершила встречу: {state}");
    assert_eq!(
        state["endedMarker"], true,
        "маркер конца встречи не появился в DOM: {state}"
    );
}
