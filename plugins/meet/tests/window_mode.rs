//! СТЕНД ИЗМЕРЕНИЯ: как режим окна влияет на непрерывность захвата.
//!
//! ```bash
//! FORGE_MEET_E2E=1 FORGE_MEET_WINDOW_BENCH=1 \
//!   cargo test -p forge-meet --test window_mode -- --nocapture --test-threads=1
//! ```
//!
//! # Зачем этот стенд существует
//!
//! Ф1 измерила неприятное: **`offscreen` не прячет окно на macOS.** Запрос
//! `left: -4000` для окна 1280×800 оконный сервер подрезает до фактических
//! `left: -930, top: 30` — на экране остаётся полоса около 350 пикселей, и всю
//! часовую встречу она висит поверх работы пользователя. Требование «браузер
//! не мешает работать» при этом нарушено по сути, хотя фокус формально не
//! украден.
//!
//! Свёрнутое окно (`minimized`) исчезает с экрана целиком. Но свёрнутое окно
//! occluded, а Chrome троттлит таймеры рендерера occluded-окна **до одной
//! секунды**. План это предвидел и вынес VAD в `AudioWorkletProcessor`
//! (аудиопоток, троттлингу не подвержен), плюс стоят три флага
//! `--disable-*-throttling`. То есть риск, вероятно, закрыт — **но это надо
//! ИЗМЕРИТЬ, а не предположить.**
//!
//! # Что именно меряется
//!
//! Пять минут записи на свёрнутом окне и на окне за экраном:
//!
//! | величина | почему она |
//! |---|---|
//! | частота кадров захвата | worklet отдаёт кадр раз в 20 мс; просадка означает, что аудиопоток всё-таки троттлится |
//! | максимальный разрыв между кадрами | средняя частота скрывает секундные провалы, а именно они и есть троттлинг |
//! | разрывы в сэмповых смещениях | пропуск смещения — это дырка в WAV, то есть потерянный звук |
//! | работает ли `Page.captureScreenshot` | у свёрнутого окна снимок может отказать, а «показать, что видит бот» — несущая функция |
//!
//! # Почему это тест, а не разовый скрипт
//!
//! Измерение придётся повторять: на другой версии Chrome, на другой машине, на
//! другой версии macOS. Разовый скрипт к тому моменту потеряется, а его
//! результат останется в чьей-то памяти как «вроде работало».

use std::time::Duration;

use forge_cdp::inject::BindingReceiver;
use forge_cdp::launch::{launch, LaunchConfig, WindowMode};
use forge_cdp::page::{CdpPage, PageSetup, ScreenshotOptions};
use forge_cdp::session::CdpBrowser;
use forge_cdp::transport::TransportConfig;
use forge_cdp::ws::WsTransport;
use forge_meet::bot::inject;
use forge_meet::bot::transport::{AudioTransport, CdpBindingTransport, PageMessage};
use forge_meet::config::MeetConfig;
use forge_meet::testing::{e2e_enabled, mock_meeting_dir, StaticServer};

/// Включено ли пятиминутное измерение.
///
/// Отдельная переменная поверх `FORGE_MEET_E2E`: обычный e2e идёт секунды, а
/// это — минуты, и тащить их в каждый прогон нельзя.
fn bench_enabled() -> bool {
    matches!(
        std::env::var("FORGE_MEET_WINDOW_BENCH").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Результат измерения одного режима.
#[derive(Debug)]
struct Measurement {
    mode: &'static str,
    /// Сколько кадров пришло.
    frames: u64,
    /// Сколько секунд шло измерение.
    seconds: f64,
    /// Максимальный разрыв между соседними кадрами, мс.
    max_gap_ms: u64,
    /// Сколько раз сэмпловые смещения дали разрыв (то есть дырку в WAV).
    offset_gaps: usize,
    /// Отдал ли свёрнутый браузер скриншот.
    screenshot_ok: bool,
    /// Фактические границы окна после установки режима.
    bounds: (i32, i32, u32, u32),
}

impl Measurement {
    fn fps(&self) -> f64 {
        if self.seconds <= 0.0 {
            return 0.0;
        }
        self.frames as f64 / self.seconds
    }

    fn report(&self) {
        eprintln!(
            "[{}] кадров {} за {:.1} с = {:.1} к/с; максимальный разрыв {} мс; \
             разрывов смещений {}; скриншот {}; окно {:?}",
            self.mode,
            self.frames,
            self.seconds,
            self.fps(),
            self.max_gap_ms,
            self.offset_gaps,
            if self.screenshot_ok { "есть" } else { "ОТКАЗАЛ" },
            self.bounds
        );
    }
}

async fn measure(mode: WindowMode, label: &'static str, secs: u64) -> Measurement {
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).unwrap();
    // Встреча заведомо длиннее измерения: страница не должна кончиться
    // раньше, чем мы досчитаем.
    let url = server.mock_meeting_url("per-track", &format!("duration={}", (secs + 60) * 1000));

    let cfg = MeetConfig {
        config_path: root.path().join("meet.toml"),
        cache_dir: root.path().join("cache"),
        db_path: root.path().join("cache/meet.db"),
        models_dir: root.path().join("cache/models"),
        ..MeetConfig::default()
    };

    let mut launch_cfg = LaunchConfig::new(root.path().join("meeting"), root.path());
    launch_cfg.window_mode = mode;
    let mut chrome = launch(&launch_cfg).await.expect("Chrome не запустился");

    let transport = WsTransport::connect(&chrome.ws_url, TransportConfig::default())
        .await
        .unwrap();
    let browser = CdpBrowser::new(transport);
    let mut page = CdpPage::open(&browser).await.unwrap();

    let inject_cfg = inject::inject_config(&cfg);
    let rx = BindingReceiver::new(page.session().subscribe(), inject_cfg.binding_name.clone());
    // Режим окна едет в `PageSetup`, а не ставится тестом руками: сворачивание
    // происходит внутри `prepare_and_navigate`, до навигации, и стенд обязан
    // мерить ровно тот путь, которым пойдёт бот.
    let setup = PageSetup {
        inject: inject_cfg,
        window_mode: mode,
        ..PageSetup::default()
    };
    page.prepare_and_navigate(&url, &setup).await.unwrap();

    let bounds = page
        .window_bounds()
        .await
        .map(|b| (b.left, b.top, b.width, b.height))
        .unwrap_or((0, 0, 0, 0));

    let mut t = CdpBindingTransport::new(rx, cfg.audio_sample_rate);
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_secs(secs);
    let mut frames = 0u64;
    let mut max_gap_ms = 0u64;
    let mut offset_gaps = 0usize;
    let mut last_at = std::time::Instant::now();
    let mut last_offset: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(left.min(Duration::from_secs(5)), t.recv()).await {
            Ok(Some(PageMessage::Audio(f))) => {
                let now = std::time::Instant::now();
                if frames > 0 {
                    max_gap_ms = max_gap_ms.max(now.duration_since(last_at).as_millis() as u64);
                }
                last_at = now;
                frames += 1;
                if let Some(prev) = last_offset.get(&f.track_id) {
                    // Разрыв смещений — это дырка в WAV, то есть потерянный
                    // звук, а не просто задержка.
                    if f.offset_samples != prev + f.len() as u64 {
                        offset_gaps += 1;
                    }
                }
                last_offset.insert(f.track_id.clone(), f.offset_samples);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    let seconds = started.elapsed().as_secs_f64();

    // Скриншот у свёрнутого окна — отдельный вопрос: «показать, что видит
    // бот» это несущая функция ассистированного входа.
    let screenshot_ok = page
        .screenshot(&ScreenshotOptions::default())
        .await
        .map(|b| !b.is_empty())
        .unwrap_or(false);

    let _ = chrome.terminate(Duration::from_secs(5)).await;

    Measurement {
        mode: label,
        frames,
        seconds,
        max_gap_ms,
        offset_gaps,
        screenshot_ok,
        bounds,
    }
}

/// ГЛАВНОЕ ИЗМЕРЕНИЕ: пять минут на свёрнутом окне.
///
/// Критерий из задачи: частота кадров worklet не просела, пропусков в PCM нет.
#[tokio::test]
async fn minimized_window_keeps_the_capture_running_for_five_minutes() {
    if !e2e_enabled() || !bench_enabled() {
        eprintln!(
            "пропуск: нужны FORGE_MEET_E2E=1 и FORGE_MEET_WINDOW_BENCH=1 \
             (измерение идёт пять минут)"
        );
        return;
    }

    let minimized = measure(WindowMode::Minimized, "minimized", 300).await;
    minimized.report();

    // Ожидаемая частота: две дорожки по кадру раз в 20 мс — сто кадров в
    // секунду. Порог намеренно мягкий: нас интересует не точность, а
    // отсутствие ПРОВАЛА в разы.
    assert!(
        minimized.fps() > 50.0,
        "частота кадров просела до {:.1} к/с — свёрнутое окно всё-таки троттлится",
        minimized.fps()
    );
    assert!(
        minimized.max_gap_ms < 1_000,
        "разрыв между кадрами {} мс: это ровно тот троттлинг таймеров, \
         которого мы боялись",
        minimized.max_gap_ms
    );
    assert_eq!(
        minimized.offset_gaps, 0,
        "в PCM появились дырки — звук теряется, а не просто задерживается"
    );
    assert!(
        minimized.screenshot_ok,
        "свёрнутое окно не отдаёт скриншот, а «показать, что видит бот» — \
         несущая функция ассистированного входа"
    );
}

/// АССИСТИРОВАННЫЙ ВХОД ИЗ ДЕФОЛТНОГО РЕЖИМА ОКНА: показали — вернули.
///
/// Живая и короткая, поэтому идёт без `FORGE_MEET_WINDOW_BENCH`.
///
/// Что здесь важно и почему это не формальность. Оконный сервер macOS
/// подрезает координаты, и «убрать за экран» на нём НЕ РАБОТАЕТ: измерено
/// `CGWindowListCopyWindowInfo` — после показа человеком и возврата окно в
/// режиме `offscreen` остаётся на экране в (−930, 30) размером 1280×800, то
/// есть полосой в 350 пикселей поверх работы пользователя до конца встречи.
/// В режиме `minimized` окон бота на экране ноль. Отсюда и дефолт.
///
/// Само исчезновение окна здесь не проверяется: `Browser.getWindowBounds` на
/// macOS отвечает `windowState: "normal"` и для свёрнутого окна (подробности —
/// в `forge-cdp`, тест `a_minimized_window_can_be_shown_to_a_human_and_hidden_again`).
/// Проверяется то, что проверяемо: снимок у свёрнутого бота есть, показ ставит
/// окно в заданные координаты, возврат не ломает ни того, ни другого.
#[tokio::test]
async fn showing_the_bot_window_and_hiding_it_again_keeps_the_capture_alive() {
    if !e2e_enabled() {
        eprintln!("пропуск: нет FORGE_MEET_E2E=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = StaticServer::start(mock_meeting_dir()).unwrap();
    let url = server.mock_meeting_url("per-track", "duration=60000");
    let cfg = MeetConfig {
        config_path: root.path().join("meet.toml"),
        cache_dir: root.path().join("cache"),
        db_path: root.path().join("cache/meet.db"),
        models_dir: root.path().join("cache/models"),
        ..MeetConfig::default()
    };
    assert_eq!(
        cfg.window_mode,
        forge_meet::config::WindowMode::Minimized,
        "дефолт режима окна изменился — переизмерь стендом, прежде чем менять тест"
    );

    let mut launch_cfg = LaunchConfig::new(root.path().join("meeting"), root.path());
    launch_cfg.window_mode = WindowMode::Minimized;
    let mut chrome = launch(&launch_cfg).await.expect("Chrome не запустился");
    let transport = WsTransport::connect(&chrome.ws_url, TransportConfig::default())
        .await
        .unwrap();
    let browser = CdpBrowser::new(transport);
    let mut page = CdpPage::open(&browser).await.unwrap();
    let inject_cfg = inject::inject_config(&cfg);
    let rx = BindingReceiver::new(page.session().subscribe(), inject_cfg.binding_name.clone());
    let setup = PageSetup {
        inject: inject_cfg,
        window_mode: WindowMode::Minimized,
        ..PageSetup::default()
    };
    page.prepare_and_navigate(&url, &setup).await.unwrap();

    // Снимок у свёрнутого бота. Сразу после сворачивания поверхности ещё нет
    // (`Not attached to an active page`), и это НОРМА: ассистированный вход
    // опрашивает снимок раз в несколько секунд.
    let mut shot_ok = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if page.screenshot(&ScreenshotOptions::default()).await.is_ok() {
            shot_ok = true;
            break;
        }
    }
    assert!(shot_ok, "свёрнутый бот не отдал ни одного снимка");

    let shown = page.set_window_visible(true).await.expect("окно не показалось");
    assert_eq!(shown.left, page.placement().visible.left);
    assert_eq!(shown.width, page.placement().visible.width);
    page.set_window_visible(false).await.expect("окно не убралось");

    // Захват обязан пережить показ и возврат: человек смотрит на бота посреди
    // встречи, и запись в этот момент не имеет права прерваться.
    let mut t = CdpBindingTransport::new(rx, cfg.audio_sample_rate);
    let mut frames = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, t.recv()).await {
            Ok(Some(PageMessage::Audio(_))) => frames += 1,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert!(
        frames > 100,
        "после показа и возврата окна кадров пришло {frames} — захват прервался"
    );

    let _ = chrome.terminate(Duration::from_secs(5)).await;
}

/// Сравнение двух режимов по минуте каждый: быстрая проверка перед тем, как
/// тратить пять минут на главное измерение.
#[tokio::test]
async fn offscreen_and_minimized_are_compared_side_by_side() {
    if !e2e_enabled() || !bench_enabled() {
        eprintln!("пропуск: нужны FORGE_MEET_E2E=1 и FORGE_MEET_WINDOW_BENCH=1");
        return;
    }

    let offscreen = measure(WindowMode::Offscreen, "offscreen", 60).await;
    offscreen.report();
    let minimized = measure(WindowMode::Minimized, "minimized", 60).await;
    minimized.report();

    // Фактические границы «за экраном» — то самое измерение Ф1: система
    // подрезает координаты, и полоса окна остаётся на экране.
    eprintln!(
        "offscreen фактически стоит на left={} (просили -4000)",
        offscreen.bounds.0
    );

    assert!(
        minimized.fps() > offscreen.fps() * 0.7,
        "свёрнутое окно даёт {:.1} к/с против {:.1} к/с за экраном — \
         разница слишком велика, чтобы считать режимы равноценными",
        minimized.fps(),
        offscreen.fps()
    );
}
