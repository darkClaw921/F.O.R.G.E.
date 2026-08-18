//! Помощники для тестов плагина.
//!
//! Модуль публичный и не спрятан за feature: интеграционные тесты
//! (`tests/*.rs`) — ОТДЕЛЬНЫЙ крейт и видят только публичное API. Здесь живут
//! замки для параллельных тестов, счётчик процессов, статический сервер
//! мок-встречи и [`mocks`] — моки внешних бинарей. Сам `ScriptedBot` — в
//! [`crate::bot::scripted`], потому что он реализует `trait MeetBot` и место
//! ему рядом с трейтом.

pub mod mocks;

/// Взаимное исключение для тестов, считающих дочерние процессы, **и для
/// тестов, которые процессы запускают**.
///
/// Тесты одного крейта идут в ОДНОМ процессе и параллельно, поэтому `sleep`,
/// запущенный соседним тестом, попадает в счётчик и роняет инвариант «`init`
/// не запускает процессов» — причём невоспроизводимо, раз в несколько
/// прогонов. Каждый такой тест обязан взять этот замок.
///
/// **Обе стороны обязательны.** Ф5 добавила моки внешних бинарей, и тесты,
/// которые их запускают, стали ровно тем шумом, от которого замок и защищает:
/// стоило им пойти без него, как счётчик увидел «10 → 10» — транзитные дети
/// соседнего теста успели родиться и умереть между двумя `ps`. Поэтому замок
/// берут и считающие, и запускающие.
///
/// Отравление замка игнорируется: паника соседнего теста уже сообщила о себе
/// сама, а превращать её во второе, невнятное падение здесь незачем.
pub fn process_count_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Взаимное исключение для ЖИВЫХ ПРОГОНОВ БРАУЗЕРА.
///
/// # Почему без него живые тесты флакуют (`forge-8ecrh`)
///
/// `FORGE_MEET_E2E=1 cargo test --test e2e_mock` идёт параллельно, и каждый
/// тест поднимает СВОЙ Chrome. На машине разработчика это пять браузеров
/// разом, и падало через раз в двух местах:
///
/// * короткий прогон (`every_source_gives_exactly_one_track`) получал ноль
///   кадров и ноль дорожек: аудиоподсистема не успевала раскрутиться, пока
///   четыре соседних Chrome дрались за процессор;
/// * проверка «после прогона не осталось живого Chrome» считала СВОИМИ
///   браузеры соседних тестов — они держат профиль с тем же именем каталога.
///
/// Измерено 17.08: с `--test-threads=1` пять тестов проходят за 58 секунд,
/// без флага — два падения. Требовать флаг в шапке файла бесполезно: его
/// забудут ровно в тот прогон, когда он нужен. Замок делает то же самое
/// структурно, а правильному прогону не стоит ничего — он и так шёл по
/// очереди.
///
/// Отравление замка игнорируется по той же причине, что и в
/// [`process_count_guard`].
pub fn browser_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Взаимное исключение для тестов, мутирующих переменные окружения.
///
/// `std::env::set_var` меняет состояние ВСЕГО процесса, а тесты крейта идут в
/// одном процессе параллельно: без замка тест приоритета источников поставил
/// бы `FORGE_MEET_CACHE_DIR`, а соседний в этот момент собрал бы конфиг с
/// чужим каталогом — и падал бы раз в несколько прогонов, каждый раз в разном
/// месте.
pub fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Сколько прямых дочерних процессов у текущего процесса.
///
/// Нужно ровно одному инварианту — «`init` не запускает процессов» — и
/// проверять его так честнее, чем грепать исходники на `Command::new`: проба
/// могла бы приехать транзитивно, из чужого конструктора.
///
/// Сам вызов `ps` даёт дочерний процесс, но `output()` дожидается его
/// завершения и снимает запись, поэтому в счёт он не попадает.
///
/// Вне unix возвращает 0: инвариант там не проверяется, а не нарушается.
pub fn count_child_processes() -> usize {
    #[cfg(unix)]
    {
        let me = std::process::id().to_string();
        let Ok(out) = std::process::Command::new("ps")
            .args(["-ax", "-o", "ppid="])
            .output()
        else {
            return 0;
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.trim() == me)
            .count()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Крошечный статический HTTP-сервер для мок-встречи.
///
/// # Почему нельзя просто открыть `file://`
///
/// Три причины, и каждой хватило бы:
///
/// 1. у `file://` **непрозрачный источник**, а нам нужен настоящий origin —
///    `Browser.grantPermissions` и `Page.setBypassCSP` адресуются именно им;
/// 2. cross-origin iframe с `file://` не порождает отдельного процесса, то
///    есть OOPIF-путь инжекта на таком стенде не проверяется вовсе;
/// 3. живая площадка — это `https://`, и стенд обязан отличаться от неё в как
///    можно меньшем числе мест.
///
/// Сервер намеренно примитивен: GET, статические файлы, `Connection: close`.
/// Он обслуживает тесты, а не пользователей, и всё лишнее в нём — это лишний
/// код, который может сломаться и увести отладку в сторону.
pub struct StaticServer {
    addr: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StaticServer {
    /// Поднимает сервер на свободном порту `127.0.0.1`.
    ///
    /// Порт 0, а не фиксированный: параллельные тесты не имеют права драться
    /// за номер, а «занято» проявилось бы как случайное падение соседнего
    /// теста.
    pub fn start(root: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();

        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            while !stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // ⚠ ЗАПРОСНАЯ СТРОКА ДОЧИТЫВАЕТСЯ ЦЕЛИКОМ, а не
                        // берётся тем, что успело прийти к первому `read`.
                        //
                        // Прежняя редакция при пустом чтении подставляла путь
                        // `/`, то есть отдавала `index.html`. Под нагрузкой
                        // (load average 80 — `forge-u95zw`) первый `read`
                        // возвращал ноль байт, и запрос, которому полагался
                        // 404, получал 200: тест обхода каталогов падал на
                        // ИСПРАВНОМ сервере. Отвечать «на всякий случай»
                        // страницей — значит врать; ждём запрос или закрываем.
                        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                        let mut req = String::new();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        let mut buf = [0u8; 4096];
                        while !req.contains("\r\n") && std::time::Instant::now() < deadline {
                            match sock.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => req.push_str(&String::from_utf8_lossy(&buf[..n])),
                                Err(ref e)
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                            | std::io::ErrorKind::Interrupted
                                    ) => {}
                                Err(_) => break,
                            }
                        }
                        if !req.contains("\r\n") {
                            let _ = sock.shutdown(std::net::Shutdown::Both);
                            continue;
                        }
                        let path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .split('?')
                            .next()
                            .unwrap_or("/")
                            .to_string();
                        let rel = path.trim_start_matches('/');
                        let rel = if rel.is_empty() { "index.html" } else { rel };
                        // Обход каталогов не пускаем даже в тестовом сервере:
                        // такие послабления переживают тот файл, в котором их
                        // написали.
                        let safe = !rel.contains("..") && !rel.starts_with('/');
                        let file = root.join(rel);
                        let body = if safe { std::fs::read(&file).ok() } else { None };
                        let resp = match body {
                            Some(bytes) => {
                                let ctype = match file.extension().and_then(|e| e.to_str()) {
                                    Some("html") => "text/html; charset=utf-8",
                                    Some("js") => "text/javascript; charset=utf-8",
                                    Some("json") => "application/json; charset=utf-8",
                                    Some("css") => "text/css; charset=utf-8",
                                    _ => "application/octet-stream",
                                };
                                let mut head = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                                    bytes.len()
                                )
                                .into_bytes();
                                head.extend_from_slice(&bytes);
                                head
                            }
                            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                        };
                        let _ = sock.write_all(&resp);
                        let _ = sock.flush();
                        // Закрытие сокета, в приёмном буфере которого остались
                        // непрочитанные байты, шлёт RST вместо FIN — и клиент
                        // читает не ответ, а `Connection reset by peer`. У
                        // однопоточного сервера это выстреливает под
                        // нагрузкой, когда хвост запроса приходит после
                        // первого `read`. Поэтому перед закрытием дочитываем
                        // остаток и закрываем запись явно.
                        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                        let mut sink = [0u8; 1024];
                        while let Ok(n) = sock.read(&mut sink) {
                            if n == 0 {
                                break;
                            }
                        }
                        let _ = sock.shutdown(std::net::Shutdown::Write);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    // Прерванный и сорванный `accept` — не повод умирать.
                    // Под полным прогоном workspace ядро отдаёт `EINTR` и
                    // `ECONNABORTED` штатно, а выход из цикла означал бы, что
                    // сервер молча исчез: следующий запрос теста получал бы
                    // `Connection reset by peer`, и падал бы он не там, где
                    // ошибка. Ровно это и происходило — раз в несколько
                    // полных прогонов.
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            addr,
            stop,
            handle: Some(handle),
        })
    }

    /// Базовый адрес вида `http://127.0.0.1:PORT`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Полный URL мок-встречи в заданном режиме.
    pub fn mock_meeting_url(&self, mode: &str, extra: &str) -> String {
        let tail = if extra.is_empty() {
            String::new()
        } else {
            format!("&{extra}")
        };
        format!("{}/index.html?mode={mode}{tail}", self.base_url())
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Каталог с мок-встречей.
pub fn mock_meeting_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-meeting")
}

/// Строка встречи для тестов — все поля заполнены правдоподобно.
///
/// Живёт здесь, а не в каждом тестовом модуле по копии: у [`MeetingRow`] 22
/// поля, и добавление двадцать третьего иначе означало бы правку десятка
/// одинаковых литералов в разных файлах — то есть гарантированный разъезд.
///
/// [`MeetingRow`]: crate::db::repo::MeetingRow
pub fn sample_meeting_row(id: &str) -> crate::db::repo::MeetingRow {
    crate::db::repo::MeetingRow {
        id: id.to_string(),
        url_key: format!("telemost:telemost.yandex.ru/{id}"),
        canonical_url: format!("https://telemost.yandex.ru/j/{id}"),
        platform: crate::providers::Platform::Telemost,
        room_id: id.to_string(),
        title: None,
        status: crate::status::MeetStatus::Ready,
        status_reason: None,
        summary_status: crate::status::SummaryStatus::None,
        mode: "normal".to_string(),
        bot_name: None,
        created_at: 1,
        updated_at: 1,
        started_at: None,
        ended_at: None,
        duration_ms: None,
        meeting_dir: String::new(),
        audio_path: None,
        transcript_path: None,
        summary_path: None,
        track_mode: None,
        capture_path: None,
    }
}

/// Конфиг с ВСЕМИ путями во временном каталоге.
///
/// `FORGE_MEET_CACHE_DIR` — самая опасная переменная плагина: без неё тест,
/// создавший встречу, писал бы в `~/.forge/meet`, где лежат записи реального
/// пользователя — чужие голоса, расшифровки и саммари. Структурная сборка
/// конфига надёжнее переменной: она не зависит от того, что процесс выставил
/// в окружении соседним тестом.
pub fn hermetic_config(dir: &std::path::Path) -> crate::config::MeetConfig {
    crate::config::MeetConfig {
        config_path: dir.join("meet.toml"),
        cache_dir: dir.join("cache"),
        db_path: dir.join("cache/meet.db"),
        models_dir: dir.join("cache/models"),
        claude_cli_path: dir.join("no-such-claude"),
        ..crate::config::MeetConfig::default()
    }
}

/// Включены ли живые прогоны с Chrome.
///
/// CI без браузера обязан оставаться зелёным, поэтому e2e закрыт переменной, а
/// не «просто не запускается».
pub fn e2e_enabled() -> bool {
    matches!(
        std::env::var("FORGE_MEET_E2E").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_static_server_serves_the_mock_meeting() {
        let server = StaticServer::start(mock_meeting_dir()).unwrap();
        let url = server.mock_meeting_url("slots", "autojoin=0");
        assert!(url.contains("mode=slots") && url.contains("autojoin=0"));

        // Читаем страницу собственным сокетом: тянуть HTTP-клиент в
        // зависимости плагина ради одного теста было бы плохой сделкой.
        //
        // У сокета ЕСТЬ таймауты: без них зависший сервер вешал бы весь прогон
        // крейта, а не падал внятной строкой.
        use std::io::{Read, Write};
        let fetch = |request: &str| -> String {
            let mut sock = std::net::TcpStream::connect(server.addr).unwrap();
            sock.set_write_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            sock.write_all(request.as_bytes()).unwrap();
            let mut out = String::new();
            let _ = sock.read_to_string(&mut out);
            out
        };

        let out = fetch("GET /index.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", &out[..out.len().min(80)]);
        assert!(out.contains("Мок-встреча"), "страница не доехала целиком");

        // И обход каталогов не работает.
        let out =
            fetch("GET /../../Cargo.toml HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert!(out.starts_with("HTTP/1.1 404"), "{}", &out[..out.len().min(80)]);
    }

    /// **РЕГРЕСС `forge-u95zw`: ЗАПОЗДАВШИЙ ЗАПРОС НЕ ПРЕВРАЩАЕТСЯ В `index.html`.**
    ///
    /// Под нагрузкой (в баге — load average около 80) первый `read` сервера
    /// возвращал ноль байт, потому что запрос ещё не приехал. Прежняя редакция
    /// подставляла путь `/` и отдавала страницу — то есть 200 на запрос,
    /// которому полагался 404, и падал соседний тест на ИСПРАВНОМ коде.
    ///
    /// Воспроизводится без всякой нагрузки: соединение открывается, а запрос
    /// уходит на полсекунды позже. Прежний сервер отвечал 200, нынешний ждёт.
    #[test]
    fn a_request_that_arrives_late_still_gets_its_own_answer() {
        use std::io::{Read, Write};
        let server = StaticServer::start(mock_meeting_dir()).unwrap();

        let mut sock = std::net::TcpStream::connect(server.addr).unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        // Соединение установлено, байтов нет — ровно то состояние, в котором
        // сервер видел «пустой запрос».
        std::thread::sleep(std::time::Duration::from_millis(500));
        sock.write_all(
            b"GET /../../Cargo.toml HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut out = String::new();
        let _ = sock.read_to_string(&mut out);
        assert!(
            out.starts_with("HTTP/1.1 404"),
            "запоздавший запрос получил чужой ответ: {}",
            &out[..out.len().min(80)]
        );
    }

    /// Счётчик обязан РЕАГИРОВАТЬ: если бы он всегда возвращал ноль, инвариант
    /// «init не запускает процессов» проходил бы при любом коде.
    #[test]
    #[cfg(unix)]
    fn the_child_counter_actually_notices_a_child() {
        let _guard = process_count_guard();
        let before = count_child_processes();
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("sleep обязан быть на любой unix-машине");
        let during = count_child_processes();
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            during > before,
            "счётчик не заметил дочерний процесс ({before} → {during})"
        );
    }
}
