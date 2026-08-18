//! Запуск собственного экземпляра Chrome и получение адреса порта отладки.
//!
//! # Только свой Chrome — это не выбор, а ограничение
//!
//! С версии 136 Chrome ИГНОРИРУЕТ `--remote-debugging-port`, если не задан
//! нестандартный `--user-data-dir`. Подключиться к рабочему браузеру
//! пользователя невозможно в принципе; на машине разработки стоит Chrome 151.
//! Отсюда весь модуль: свой профиль, свой порт, свой файл тишины, своя группа
//! процессов.
//!
//! # Порт читаем из файла, а не угадываем
//!
//! `--remote-debugging-port=0` заставляет Chrome взять свободный порт и
//! записать его в `<user_data_dir>/DevToolsActivePort`. Альтернатива —
//! выбрать порт самим — это гонка: между «нашли свободный» и «Chrome его
//! занял» его может занять кто угодно, и провал будет редким и
//! невоспроизводимым.
//!
//! # Файл тишины — защита от тихой порчи живой встречи
//!
//! `--use-fake-device-for-media-stream` без `--use-file-for-fake-audio-capture`
//! даёт микрофон, ГЕНЕРИРУЮЩИЙ ПЕРИОДИЧЕСКИЙ БИП-ТОН. Бот с таким микрофоном
//! пикает живым участникам встречи. Поэтому файл тишины подкладывается
//! ВСЕГДА, а если `ffmpeg` недоступен — пишется своим кодом, потому что
//! «нет ffmpeg» не должно превращаться в «бот пикает».

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::orphans::{write_pid_file, PidRecord};
use crate::protocol::CdpError;

/// Флаги, которых в командной строке не будет НИКОГДА.
///
/// `--no-sandbox` снимает песочницу рендерера у браузера, который ходит на
/// произвольные сайты встреч; `--disable-web-security` и
/// `--disable-site-isolation-trials` ломают изоляцию источников — а мы,
/// наоборот, рассчитываем на OOPIF и на автоаттач к ним. Список
/// применяется и к `extra_args`, поэтому запрет структурный: вернуть их
/// конфигом невозможно.
pub const FORBIDDEN_ARGS: &[&str] = &[
    "--no-sandbox",
    "--disable-web-security",
    "--disable-site-isolation-trials",
];

/// Режим окна браузера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    /// Headful, окно уведено за пределы видимой зоны. Дефолт.
    ///
    /// ИЗМЕРЕНО (macOS 15, Chrome 151): «за экраном» здесь означает «ПОЧТИ за
    /// экраном» — оконный сервер подрезает запрошенный `left: -4000` до
    /// фактических `left: -930`, и на экране остаётся полоса около 350
    /// пикселей.
    #[default]
    Offscreen,
    /// Headful, окно СВЁРНУТО сразу после создания вкладки.
    ///
    /// Единственный headful-режим, при котором окна на экране не остаётся
    /// вовсе. Плата — окно occluded, и Chrome троттлит таймеры его рендерера
    /// до секунды; захват к этому готов структурно (VAD в
    /// `AudioWorkletProcessor` живёт в аудиопотоке), но величину троттлинга
    /// надо ИЗМЕРЯТЬ, а не предполагать — стенд в
    /// `plugins/meet/tests/window_mode.rs`.
    Minimized,
    /// Headful, окно на экране: ассистированный вход, когда человеку надо
    /// пройти капчу или лобби руками.
    Visible,
    /// `--headless=new`. НЕ дефолт: UA содержит `HeadlessChrome` (повод для
    /// антибот-отказа), поведение WebRTC-аудио на macOS в этом режиме не
    /// проверено, и главное — headless-окно нельзя показать человеку, а показ
    /// окна это несущая функция ассистированного входа, не украшение.
    Headless,
}

/// Параметры запуска.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Каталог встречи: сюда пишутся `chrome.pid`, `chrome.log` и (по
    /// умолчанию) профиль.
    pub meeting_dir: PathBuf,
    /// Профиль браузера. Обязателен с Chrome 136+, иначе порт отладки
    /// игнорируется.
    pub user_data_dir: PathBuf,
    /// Файл тишины для фейкового микрофона. Лежит в КЭШ-каталоге, общем для
    /// всех встреч: он одинаковый, весит 11 МБ и создаётся один раз.
    pub silence_wav: PathBuf,
    /// Исполняемый файл Chrome. `None` — искать по известным путям.
    pub chrome_binary: Option<PathBuf>,
    /// Путь к `ffmpeg` для генерации файла тишины. `None` — сначала искать в
    /// `PATH`, а не найдя, писать WAV своим кодом.
    pub ffmpeg_binary: Option<PathBuf>,
    /// Режим окна.
    pub window_mode: WindowMode,
    /// Размер окна.
    pub window_size: (u32, u32),
    /// Куда ставить окно в режиме [`WindowMode::Offscreen`].
    ///
    /// Позиция задаётся уже при СТАРТЕ (`--window-position`), а не только
    /// потом через `Browser.setWindowBounds`: иначе окно встречи успевает
    /// мигнуть поверх работы пользователя. Насколько далеко оконный сервер
    /// macOS позволит увести окно — вопрос платформы, поэтому значение
    /// вынесено в поле.
    pub offscreen_position: (i32, i32),
    /// Стартовый URL. Всегда `about:blank`: настоящая навигация идёт через
    /// `Page.navigate` ПОСЛЕ инжекта, иначе патчи опоздают.
    ///
    /// Используется, ТОЛЬКО если [`LaunchConfig::no_startup_window`]
    /// выключен: позиционный URL — это как раз просьба открыть окно.
    pub initial_url: String,
    /// `--no-startup-window`: не открывать окно при запуске.
    ///
    /// Это решение проблемы КРАЖИ ФОКУСА, а не видимости окна — их легко
    /// перепутать. `window_mode = offscreen` уводит окно за пределы экрана,
    /// но на macOS запуск приложения с окном АКТИВИРУЕТ его, и пользователь,
    /// работающий параллельно, теряет ввод в момент старта бота.
    ///
    /// ИЗМЕРЕНО (macOS 15, Chrome 151, фронт определялся `lsappinfo front`):
    /// прямой запуск с окном переводит фронт с текущего приложения на
    /// `com.google.Chrome` в первую же секунду и оставляет его там; тот же
    /// запуск с `--no-startup-window` фронт НЕ МЕНЯЕТ вовсе. Файл
    /// `DevToolsActivePort` при этом пишется как обычно — порт отладки
    /// живёт на уровне браузера и от наличия окна не зависит.
    ///
    /// Почему не `open -g -n -a "Google Chrome" --args …`: LaunchServices
    /// возвращает управление сразу и не отдаёт дескриптор процесса, а он
    /// несущий — на нём держится и `chrome.pid`, и убийство группы, и
    /// определение «Chrome умер, не создав порт». Флаг Chrome даёт тот же
    /// эффект, ничего не отнимая.
    pub no_startup_window: bool,
    /// Сколько ждать появления `DevToolsActivePort`.
    pub devtools_port_timeout: Duration,
    /// Шаг опроса файла `DevToolsActivePort`.
    pub devtools_poll_interval: Duration,
    /// `--mute-audio`: звук встречи не льётся в колонки пользователя.
    ///
    /// Вынесено в поле, а не зашито, НАМЕРЕННО: гейт фазы 0 оставил открытым
    /// вопрос, не глушит ли этот флаг данные для WebAudio и `MediaRecorder`,
    /// и сделал его приёмочным пунктом фазы 3 («запуск с флагом и без,
    /// сравнение того, что доехало по тапам»). Переключатель нужен, чтобы
    /// измерение не требовало правки кода.
    pub mute_audio: bool,
    /// Дополнительные флаги. Запрещённые из [`FORBIDDEN_ARGS`] отфильтруются.
    pub extra_args: Vec<String>,
}

impl LaunchConfig {
    /// Конфигурация по умолчанию.
    ///
    /// Два каталога, а не один: `meeting_dir` живёт и умирает вместе со
    /// встречей (профиль, pid, лог), `cache_dir` переживает встречи и хранит
    /// файл тишины — переиздавать 11 МБ нулей на каждую встречу незачем.
    pub fn new(meeting_dir: impl Into<PathBuf>, cache_dir: impl AsRef<Path>) -> Self {
        let meeting_dir = meeting_dir.into();
        Self {
            user_data_dir: meeting_dir.join("chrome-profile"),
            silence_wav: cache_dir.as_ref().join("silence.wav"),
            meeting_dir,
            chrome_binary: None,
            ffmpeg_binary: None,
            window_mode: WindowMode::default(),
            window_size: (1280, 800),
            offscreen_position: (-4000, 0),
            initial_url: "about:blank".to_string(),
            no_startup_window: true,
            devtools_port_timeout: Duration::from_secs(30),
            devtools_poll_interval: Duration::from_millis(50),
            mute_audio: true,
            extra_args: Vec::new(),
        }
    }
}

/// Запущенный браузер.
#[derive(Debug)]
pub struct ChromeProcess {
    /// PID процесса-лидера. Он же идентификатор группы процессов: браузер
    /// стартовал через `setsid`, поэтому `pgid == pid`.
    pub pid: i32,
    /// Адрес ws-эндпоинта браузера, прочитанный из `DevToolsActivePort`.
    pub ws_url: String,
    /// Профиль — он же признак «наш процесс» при подметании сирот.
    pub user_data_dir: PathBuf,
    /// Каталог встречи (в нём лежит `chrome.pid`).
    pub meeting_dir: PathBuf,
    child: tokio::process::Child,
}

impl ChromeProcess {
    /// Гасит браузер: `SIGTERM` всей группе, затем `SIGKILL` по истечении
    /// отсрочки. Именно группе — Chrome разводит рендереры и GPU-процесс
    /// отдельными процессами, и убийство одного лидера оставило бы их жить.
    pub async fn terminate(&mut self, grace: Duration) -> Result<(), CdpError> {
        terminate_child_group(&mut self.child, self.pid, grace, Duration::from_millis(50)).await;
        let pid_file = self.meeting_dir.join(crate::orphans::PID_FILE_NAME);
        if pid_file.exists() {
            let _ = std::fs::remove_file(&pid_file);
        }
        Ok(())
    }

    /// Уже завершился ли процесс сам (например, Chrome упал).
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        // Последняя сетка: штатный путь — `terminate`, но если ChromeProcess
        // роняют по ошибке или паникой, живой Chrome с захваченным
        // микрофоном оставлять нельзя. Это НЕ замена подметальщику сирот:
        // от SIGKILL по самому devforge деструкторы не спасают, см.
        // `orphans::sweep_orphans`.
        if self.pid > 0 {
            unsafe {
                libc::killpg(self.pid, libc::SIGTERM);
            }
        }
    }
}

/// Гасит группу СОБСТВЕННОГО потомка и дожидается его.
///
/// Отличается от [`crate::orphans::terminate_group`] тем, что мёртвый
/// потомок остаётся зомби, пока его не забрал `waitpid`, а зомби для
/// `kill(pid, 0)` неотличим от живого. Проверять надо `try_wait`, иначе
/// ожидание всегда доходит до `SIGKILL` и «отсрочка» превращается в
/// фиксированную паузу.
async fn terminate_child_group(
    child: &mut tokio::process::Child,
    pid: i32,
    grace: Duration,
    poll: Duration,
) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    crate::orphans::signal_group(pid, libc::SIGTERM);
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        tokio::time::sleep(poll).await;
    }
    tracing::warn!(pid, "forge-cdp: Chrome не умер по SIGTERM, шлём SIGKILL группе");
    crate::orphans::signal_group(pid, libc::SIGKILL);
    let _ = child.wait().await;
}

/// Собирает командную строку Chrome.
///
/// Вынесена отдельной чистой функцией именно ради теста: инварианты «файл
/// тишины присутствует всегда» и «запрещённых флагов нет ни при каких
/// настройках» проверяются без запуска браузера.
pub fn build_args(cfg: &LaunchConfig) -> Vec<String> {
    let mut args: Vec<String> = vec![
        // Обязателен с Chrome 136+: без нестандартного профиля
        // --remote-debugging-port просто игнорируется.
        format!("--user-data-dir={}", cfg.user_data_dir.display()),
        // 0 = «возьми свободный и запиши в DevToolsActivePort».
        "--remote-debugging-port=0".to_string(),
        // Авто-грант микрофона и камеры: диалога разрешений, который некому
        // нажать, не будет.
        "--use-fake-ui-for-media-stream".to_string(),
        // Виртуальный микрофон вместо реального: бот не транслирует комнату
        // пользователя.
        "--use-fake-device-for-media-stream".to_string(),
        // БЕЗ ЭТОГО ФЛАГА ФЕЙКОВЫЙ МИКРОФОН ПИКАЕТ В ЖИВУЮ ВСТРЕЧУ.
        format!(
            "--use-file-for-fake-audio-capture={}",
            cfg.silence_wav.display()
        ),
        // AudioContext стартует в состоянии running, а не suspended: нажать
        // «первый жест пользователя» на странице встречи некому.
        "--autoplay-policy=no-user-gesture-required".to_string(),
        // Три флага против троттлинга фоновой вкладки: окно уведено за экран
        // и формально не видно, а таймеры захвата обязаны идти.
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
        // Убирает navigator.webdriver — самый дешёвый признак автоматизации.
        "--disable-blink-features=AutomationControlled".to_string(),
        // Свежий профиль иначе показывает мастер первого запуска и диалог
        // «сделать браузером по умолчанию»: они перехватывают фокус и стоят
        // поверх страницы встречи.
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        format!("--window-size={},{}", cfg.window_size.0, cfg.window_size.1),
    ];

    if cfg.mute_audio {
        args.push("--mute-audio".to_string());
    }
    match cfg.window_mode {
        WindowMode::Headless => args.push("--headless=new".to_string()),
        WindowMode::Offscreen => args.push(format!(
            "--window-position={},{}",
            cfg.offscreen_position.0, cfg.offscreen_position.1
        )),
        // Свернуть окно флагом командной строки нельзя: `--start-minimized` у
        // Chrome нет. Состояние ставится по протоколу сразу после создания
        // вкладки (`CdpPage::minimize`), а до этого момента окна и так нет —
        // его не открывает `--no-startup-window`.
        WindowMode::Minimized => {}
        WindowMode::Visible => {}
    }

    for extra in &cfg.extra_args {
        if is_forbidden(extra) {
            tracing::warn!(
                arg = %extra,
                "forge-cdp: запрещённый флаг Chrome отброшен из extra_args"
            );
            continue;
        }
        args.push(extra.clone());
    }

    if cfg.no_startup_window {
        // Окна при старте нет — значит, нечего активировать, и фокус
        // пользователя остаётся там, где был. Вкладку встречи заведёт
        // `Target.createTarget{background:true}` уже по протоколу.
        args.push("--no-startup-window".to_string());
    } else {
        // Стартовый URL — позиционным аргументом, последним. Он же и есть
        // просьба открыть окно, поэтому вместе с --no-startup-window не идёт.
        args.push(cfg.initial_url.clone());
    }
    args
}

fn is_forbidden(arg: &str) -> bool {
    FORBIDDEN_ARGS
        .iter()
        .any(|f| arg == *f || arg.starts_with(&format!("{f}=")))
}

/// Создаёт файл тишины, если его ещё нет.
///
/// Идемпотентна: существующий валидный WAV не переписывается — файл общий для
/// всех встреч, и переиздавать 11 МБ на каждую незачем.
///
/// Порядок попыток: `ffmpeg` (как в плане), затем собственный писатель WAV.
/// Второй путь обязателен: отсутствие `ffmpeg` не имеет права превращаться в
/// пикающего бота.
///
/// Возвращает `true`, если файл был создан этим вызовом.
pub fn ensure_silence_wav(path: &Path, ffmpeg: Option<&Path>) -> Result<bool, CdpError> {
    if is_valid_wav(path) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp.wav");
    let _ = std::fs::remove_file(&tmp);

    let ffmpeg_path = ffmpeg
        .map(Path::to_path_buf)
        .or_else(|| which("ffmpeg").filter(|_| ffmpeg.is_none()));

    if let Some(ff) = ffmpeg_path {
        // Ровно команда плана. Без суффикса %noloop у
        // --use-file-for-fake-audio-capture — файл должен зациклиться, а не
        // кончиться через минуту тишины и вернуть бип.
        let out = std::process::Command::new(&ff)
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo",
                "-t",
                "60",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&tmp)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match out {
            Ok(status) if status.success() && is_valid_wav(&tmp) => {
                std::fs::rename(&tmp, path)?;
                tracing::info!(path = %path.display(), "forge-cdp: файл тишины создан через ffmpeg");
                return Ok(true);
            }
            other => {
                let _ = std::fs::remove_file(&tmp);
                tracing::warn!(
                    ffmpeg = %ff.display(),
                    result = ?other,
                    "forge-cdp: ffmpeg не сделал файл тишины, пишем WAV своим кодом"
                );
            }
        }
    }

    write_silence_wav(&tmp)?;
    std::fs::rename(&tmp, path)?;
    tracing::info!(path = %path.display(), "forge-cdp: файл тишины записан собственным писателем");
    Ok(true)
}

/// Длительность файла тишины в секундах.
const SILENCE_SECONDS: u32 = 60;
/// Частота дискретизации файла тишины (совпадает с параметром `anullsrc`).
const SILENCE_RATE: u32 = 48_000;
/// Каналов в файле тишины.
const SILENCE_CHANNELS: u16 = 2;

/// Пишет 60 секунд тишины 48 кГц / стерео / PCM16 без внешних инструментов.
///
/// Формат совпадает с тем, что даёт команда `ffmpeg` из плана, — чтобы
/// поведение фейкового микрофона не зависело от того, каким путём файл
/// появился.
fn write_silence_wav(path: &Path) -> Result<(), CdpError> {
    let bits = 16u16;
    let block_align = SILENCE_CHANNELS * bits / 8;
    let byte_rate = SILENCE_RATE * u32::from(block_align);
    let data_len = byte_rate * SILENCE_SECONDS;

    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::with_capacity(1 << 16, file);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // размер fmt-чанка
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&SILENCE_CHANNELS.to_le_bytes())?;
    w.write_all(&SILENCE_RATE.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;

    let zeros = vec![0u8; 1 << 16];
    let mut left = data_len as usize;
    while left > 0 {
        let n = left.min(zeros.len());
        w.write_all(&zeros[..n])?;
        left -= n;
    }
    w.flush()?;
    Ok(())
}

fn is_valid_wav(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > 44 => {}
        _ => return false,
    }
    let mut head = [0u8; 12];
    match std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)) {
        Ok(()) => &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE",
        Err(_) => false,
    }
}

/// Ищет исполняемый файл в `PATH`.
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

/// Кандидаты на роль Chrome, в порядке предпочтения.
///
/// Переменные окружения идут первыми: на машине пользователя может стоять
/// нестандартная сборка, и заставлять его двигать файлы неправильно.
pub fn chrome_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["FORGE_CHROME", "CHROME_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            if !v.is_empty() {
                out.push(PathBuf::from(v));
            }
        }
    }
    out.extend(
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
        .iter()
        .map(PathBuf::from),
    );
    for bin in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        if let Some(p) = which(bin) {
            out.push(p);
        }
    }
    out
}

/// Находит Chrome или объясняет, где искали.
pub fn find_chrome() -> Result<PathBuf, CdpError> {
    let candidates = chrome_candidates();
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(CdpError::ChromeNotFound(format!(
        "проверены пути: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Имя файла, в который Chrome пишет порт отладки.
pub const DEVTOOLS_PORT_FILE: &str = "DevToolsActivePort";

/// Запускает Chrome и возвращает адрес ws-эндпоинта браузера.
pub async fn launch(cfg: &LaunchConfig) -> Result<ChromeProcess, CdpError> {
    let chrome = match &cfg.chrome_binary {
        Some(p) => p.clone(),
        None => find_chrome()?,
    };

    std::fs::create_dir_all(&cfg.meeting_dir)?;
    std::fs::create_dir_all(&cfg.user_data_dir)?;
    ensure_silence_wav(&cfg.silence_wav, cfg.ffmpeg_binary.as_deref())?;

    // Файл порта от прошлого запуска обязан исчезнуть ДО старта: иначе
    // ожидание немедленно прочитает мёртвый порт и мы будем ломиться в
    // никуда.
    let port_file = cfg.user_data_dir.join(DEVTOOLS_PORT_FILE);
    let _ = std::fs::remove_file(&port_file);

    let args = build_args(cfg);
    let log_path = cfg.meeting_dir.join("chrome.log");
    let log = std::fs::File::create(&log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = tokio::process::Command::new(&chrome);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        // kill_on_drop не ставим: он бьёт только лидера и только при
        // выполнении деструкторов. Гашение идёт по группе — `terminate` и
        // `Drop for ChromeProcess`, а от SIGKILL по devforge спасает
        // подметальщик сирот на следующем старте.
        .kill_on_drop(false);

    // Своя группа процессов. Без неё убить Chrome целиком нельзя: рендереры,
    // GPU-процесс и утилиты — отдельные процессы, и `kill` по лидеру оставит
    // их держать микрофон и профиль.
    //
    // SAFETY: setsid(2) async-signal-safe и в промежутке между fork и exec
    // вызывать его законно; ничего, кроме него, в замыкании нет — аллокаций,
    // логов и блокировок здесь быть не должно. Ровно тот же приём применяет
    // tmux-web/src/daemon.rs.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| {
        CdpError::Launch(format!(
            "не удалось запустить {}: {e}",
            chrome.display()
        ))
    })?;
    let pid = child.id().ok_or_else(|| {
        CdpError::Launch("Chrome завершился раньше, чем сообщил свой pid".to_string())
    })? as i32;

    write_pid_file(
        &cfg.meeting_dir,
        &PidRecord {
            pid,
            user_data_dir: cfg.user_data_dir.clone(),
            started_unix: now_unix(),
        },
    )?;

    let ws_url = match wait_for_ws_url(
        &port_file,
        &mut child,
        cfg.devtools_port_timeout,
        cfg.devtools_poll_interval,
        &log_path,
    )
    .await
    {
        Ok(url) => url,
        Err(e) => {
            // Не оставляем половину запуска: полудохлый Chrome с профилем и
            // микрофоном хуже отсутствия браузера.
            terminate_child_group(
                &mut child,
                pid,
                Duration::from_secs(2),
                Duration::from_millis(50),
            )
            .await;
            let _ = std::fs::remove_file(cfg.meeting_dir.join(crate::orphans::PID_FILE_NAME));
            return Err(e);
        }
    };

    tracing::info!(
        pid,
        ws = %ws_url,
        profile = %cfg.user_data_dir.display(),
        mode = ?cfg.window_mode,
        "forge-cdp: Chrome запущен"
    );

    Ok(ChromeProcess {
        pid,
        ws_url,
        user_data_dir: cfg.user_data_dir.clone(),
        meeting_dir: cfg.meeting_dir.clone(),
        child,
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Ждёт появления `DevToolsActivePort` и превращает его в адрес ws.
///
/// Ожидание файла, а не опрос фиксированного порта: порт назначает Chrome.
/// Параллельно следим за самим процессом — если он успел умереть (не нашёл
/// профиль, конфликт флагов), ждать файл целую минуту бессмысленно, и в
/// ошибку кладётся хвост `chrome.log`, иначе диагностировать нечем.
async fn wait_for_ws_url(
    port_file: &Path,
    child: &mut tokio::process::Child,
    timeout: Duration,
    poll: Duration,
    log_path: &Path,
) -> Result<String, CdpError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(url) = read_ws_url(port_file) {
            return Ok(url);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(CdpError::Launch(format!(
                "Chrome завершился с {status} не создав {}; хвост лога:\n{}",
                DEVTOOLS_PORT_FILE,
                tail_of(log_path, 2000)
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CdpError::Launch(format!(
                "{} не появился за {} мс; хвост лога:\n{}",
                DEVTOOLS_PORT_FILE,
                timeout.as_millis(),
                tail_of(log_path, 2000)
            )));
        }
        tokio::time::sleep(poll).await;
    }
}

/// Разбирает `DevToolsActivePort`: первая строка — порт, вторая — путь
/// браузерного эндпоинта (`/devtools/browser/<uuid>`).
///
/// Возвращает `None` и на отсутствие файла, и на недописанный файл: Chrome
/// создаёт его и дописывает вторую строку не атомарно, поэтому увидеть
/// половину — нормальная ситуация, а не ошибка.
pub fn read_ws_url(port_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(port_file).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    if path.is_empty() {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

fn tail_of(path: &Path, bytes: usize) -> String {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return format!("(лог недоступен: {e})"),
    };
    let start = data.len().saturating_sub(bytes);
    String::from_utf8_lossy(&data[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg_in(dir: &TempDir) -> LaunchConfig {
        LaunchConfig::new(dir.path().join("meeting"), dir.path().join("cache"))
    }

    #[test]
    fn silence_file_is_always_in_argv() {
        let tmp = TempDir::new().unwrap();
        for mode in [WindowMode::Offscreen, WindowMode::Visible, WindowMode::Headless] {
            for mute in [true, false] {
                let mut cfg = cfg_in(&tmp);
                cfg.window_mode = mode;
                cfg.mute_audio = mute;
                let args = build_args(&cfg);
                assert!(
                    args.iter()
                        .any(|a| a.starts_with("--use-file-for-fake-audio-capture=")),
                    "режим {mode:?}: без файла тишины фейковый микрофон пикает в живую встречу"
                );
                assert!(args
                    .iter()
                    .any(|a| a == "--use-fake-device-for-media-stream"));
                assert!(args.iter().any(|a| a == "--use-fake-ui-for-media-stream"));
            }
        }
    }

    #[test]
    fn forbidden_args_never_appear_under_any_settings() {
        let tmp = TempDir::new().unwrap();
        for mode in [WindowMode::Offscreen, WindowMode::Visible, WindowMode::Headless] {
            let mut cfg = cfg_in(&tmp);
            cfg.window_mode = mode;
            // Пытаемся протащить запрещённое конфигом — не должно пройти.
            cfg.extra_args = vec![
                "--no-sandbox".into(),
                "--disable-web-security".into(),
                "--disable-site-isolation-trials".into(),
                "--disable-web-security=1".into(),
                "--lang=ru".into(),
            ];
            let args = build_args(&cfg);
            for forbidden in FORBIDDEN_ARGS {
                assert!(
                    !args.iter().any(|a| a.starts_with(forbidden)),
                    "{forbidden} просочился в argv (режим {mode:?})"
                );
            }
            assert!(args.iter().any(|a| a == "--lang=ru"), "безобидный extra-флаг должен доезжать");
        }
    }

    #[test]
    fn headless_flag_only_in_headless_mode_and_url_is_last() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = cfg_in(&tmp);
        cfg.window_mode = WindowMode::Offscreen;
        let args = build_args(&cfg);
        assert!(!args.iter().any(|a| a.starts_with("--headless")));
        // Окно уводится за экран уже при старте, а не после подключения:
        // иначе оно мигает поверх работы пользователя.
        assert!(args.iter().any(|a| a == "--window-position=-4000,0"));
        assert_eq!(args.last().unwrap(), "--no-startup-window");

        cfg.window_mode = WindowMode::Visible;
        assert!(!build_args(&cfg)
            .iter()
            .any(|a| a.starts_with("--window-position")));

        cfg.window_mode = WindowMode::Headless;
        let args = build_args(&cfg);
        assert!(args.iter().any(|a| a == "--headless=new"));
        assert!(!args.iter().any(|a| a.starts_with("--window-position")));
    }

    /// Браузер не имеет права отбирать фокус у пользователя при старте.
    ///
    /// Это ОТДЕЛЬНОЕ требование от «окно за экраном»: на macOS запуск
    /// приложения с окном активирует его, и пользователь, работающий
    /// параллельно, теряет ввод. Лечится отсутствием окна при старте.
    #[test]
    fn startup_opens_no_window_so_focus_is_not_stolen() {
        let tmp = TempDir::new().unwrap();
        for mode in [WindowMode::Offscreen, WindowMode::Visible, WindowMode::Headless] {
            let mut cfg = cfg_in(&tmp);
            cfg.window_mode = mode;
            let args = build_args(&cfg);
            assert!(
                args.iter().any(|a| a == "--no-startup-window"),
                "режим {mode:?}: без --no-startup-window Chrome активируется поверх работы пользователя"
            );
            assert!(
                !args.iter().any(|a| a == "about:blank"),
                "позиционный URL — это просьба открыть окно, вместе с --no-startup-window он не идёт"
            );
        }
        // Выключатель на случай, когда окно при старте всё-таки нужно.
        let mut cfg = cfg_in(&tmp);
        cfg.no_startup_window = false;
        let args = build_args(&cfg);
        assert!(!args.iter().any(|a| a == "--no-startup-window"));
        assert_eq!(args.last().unwrap(), "about:blank");
    }

    #[test]
    fn mute_audio_is_a_switch_for_phase3_measurement() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = cfg_in(&tmp);
        assert!(build_args(&cfg).iter().any(|a| a == "--mute-audio"));
        cfg.mute_audio = false;
        assert!(!build_args(&cfg).iter().any(|a| a == "--mute-audio"));
    }

    #[test]
    fn silence_wav_is_written_without_ffmpeg_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache").join("silence.wav");
        // ffmpeg заведомо отсутствует — путь фолбэка.
        let created = ensure_silence_wav(&path, Some(Path::new("/nonexistent/ffmpeg"))).unwrap();
        assert!(created);
        let meta = std::fs::metadata(&path).unwrap();
        // 60 с * 48000 * 2 канала * 2 байта + 44 байта заголовка.
        assert_eq!(meta.len(), 60 * 48_000 * 2 * 2 + 44);

        let head = std::fs::read(&path).unwrap();
        assert_eq!(&head[0..4], b"RIFF");
        assert_eq!(&head[8..12], b"WAVE");
        // Данные — именно тишина, а не бип.
        assert!(head[44..1044].iter().all(|b| *b == 0));

        // Повторный вызов не переписывает файл.
        let created_again = ensure_silence_wav(&path, Some(Path::new("/nonexistent/ffmpeg"))).unwrap();
        assert!(!created_again);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), meta.len());
        // Временный файл за собой не оставляем.
        assert!(!path.with_extension("tmp.wav").exists());
    }

    #[test]
    fn silence_wav_via_ffmpeg_when_available() {
        let Some(ffmpeg) = which("ffmpeg") else {
            eprintln!("ffmpeg не найден — ветка ffmpeg не проверяется");
            return;
        };
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("silence.wav");
        assert!(ensure_silence_wav(&path, Some(&ffmpeg)).unwrap());
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], b"RIFF");
        assert!(data.len() > 11_000_000, "60 с стерео 48 кГц PCM16 весит ~11,5 МБ");
        assert!(data[1000..2000].iter().all(|b| *b == 0));
    }

    #[test]
    fn ws_url_is_read_only_when_file_is_complete() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join(DEVTOOLS_PORT_FILE);
        assert_eq!(read_ws_url(&f), None);
        // Chrome сначала пишет порт, потом путь: половину файла читать нельзя.
        std::fs::write(&f, "51234\n").unwrap();
        assert_eq!(read_ws_url(&f), None);
        std::fs::write(&f, "51234\n/devtools/browser/9f0e\n").unwrap();
        assert_eq!(
            read_ws_url(&f).as_deref(),
            Some("ws://127.0.0.1:51234/devtools/browser/9f0e")
        );
    }

    #[test]
    fn config_keeps_profile_in_meeting_and_silence_in_cache() {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg_in(&tmp);
        assert!(cfg.user_data_dir.starts_with(tmp.path().join("meeting")));
        assert!(cfg.silence_wav.starts_with(tmp.path().join("cache")));
        assert_eq!(cfg.window_mode, WindowMode::Offscreen);
    }
}
