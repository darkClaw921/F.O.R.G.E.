//! `chrome.pid`, сигналы группе процессов и подметание осиротевших Chrome.
//!
//! # Проблема
//!
//! `kill_on_drop` не спасает от `SIGKILL` по самому devforge: процесс умирает,
//! деструкторы не выполняются, Chrome остаётся жить — держит микрофон, держит
//! профиль встречи и продолжает сидеть в конференции. Пользователь при этом
//! видит «запись остановлена».
//!
//! # Решение
//!
//! Chrome стартует в СВОЕЙ группе процессов (`setsid` в `pre_exec`), а его
//! `pid` и `user_data_dir` ложатся в `<meeting_dir>/chrome.pid`. На старте
//! плагина обходятся все `chrome.pid` в кэше, и по каждому делается ДВОЙНАЯ
//! проверка:
//!
//! 1. процесс жив;
//! 2. его `argv` содержит НАШ `--user-data-dir`.
//!
//! Вторая проверка обязательна. PID переиспользуются операционной системой, и
//! убийство по одному лишь живому pid однажды прибьёт посторонний процесс
//! пользователя — редко, невоспроизводимо и разрушительно.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::protocol::CdpError;

/// Имя файла с описанием запущенного браузера внутри каталога встречи.
pub const PID_FILE_NAME: &str = "chrome.pid";

/// Содержимое `chrome.pid`.
///
/// Хранится JSON'ом, а не одним числом: pid без `user_data_dir` бесполезен —
/// именно профиль отличает наш Chrome от чужого процесса, унаследовавшего
/// номер.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidRecord {
    /// PID лидера группы (он же pgid: браузер стартовал через `setsid`).
    pub pid: i32,
    /// Профиль браузера — признак «это наш процесс».
    pub user_data_dir: PathBuf,
    /// Время старта, unix-секунды. Диагностическое поле: по нему видно,
    /// сирота ли это часовой давности или файл, забытый месяц назад.
    pub started_unix: u64,
}

/// Пишет `chrome.pid` в каталог встречи.
///
/// Через временный файл и `rename`: подметальщик читает эти файлы у процессов,
/// которые могли умереть в любой момент, и половинчатый JSON заставил бы его
/// пропустить живую сироту.
pub fn write_pid_file(meeting_dir: &Path, record: &PidRecord) -> Result<(), CdpError> {
    std::fs::create_dir_all(meeting_dir)?;
    let path = meeting_dir.join(PID_FILE_NAME);
    let tmp = meeting_dir.join(format!("{PID_FILE_NAME}.tmp"));
    let data = serde_json::to_vec_pretty(record)
        .map_err(|e| CdpError::Launch(format!("не сериализуется {PID_FILE_NAME}: {e}")))?;
    std::fs::write(&tmp, &data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Читает `chrome.pid` (принимает и путь к файлу, и каталог встречи).
pub fn read_pid_file(path: &Path) -> Result<PidRecord, CdpError> {
    let file = if path.is_dir() {
        path.join(PID_FILE_NAME)
    } else {
        path.to_path_buf()
    };
    let data = std::fs::read(&file)?;
    serde_json::from_slice(&data).map_err(|e| {
        CdpError::Malformed(format!("{} не разбирается: {e}", file.display()))
    })
}

/// Жив ли процесс.
///
/// `kill(pid, 0)` не шлёт сигнала, а проверяет право его послать: успех
/// означает «процесс есть», `EPERM` — «процесс есть, но чужой», `ESRCH` —
/// «процесса нет». Чужой процесс тоже считается живым: решение об убийстве
/// принимает вторая проверка, по argv.
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // errno берём через std::io::Error — прямой доступ к __error/__errno_location
    // платформозависим.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Полный `argv` процесса по данным `ps`.
///
/// Через `ps -p <pid> -o args=`, а не через чтение `/proc`: на macOS `/proc`
/// нет вовсе, а `ps` есть везде и одинаково.
pub fn process_argv(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Наш ли это процесс: argv содержит именно наш профиль.
///
/// Сравнивается путь профиля, а не подстрока «chrome»: два бота на разных
/// встречах отличаются только профилем, и подметальщик первого не имеет права
/// убить второго.
pub fn argv_matches_profile(argv: &str, user_data_dir: &Path) -> bool {
    let needle = user_data_dir.to_string_lossy();
    if needle.is_empty() {
        return false;
    }
    argv.contains(needle.as_ref())
}

/// Шлёт сигнал ВСЕЙ группе процессов.
///
/// Chrome разводит рендереры, GPU-процесс и утилиты отдельными процессами;
/// сигнал одному лидеру оставил бы их жить. Группа берётся не на веру:
/// `killpg` идёт только если `getpgid(pid) == pid`, то есть процесс
/// действительно лидер своей группы (наш `setsid` отработал). Иначе бьём
/// одиночным `kill` — сигналить чужой группе, в которой процесс оказался по
/// стечению обстоятельств, недопустимо.
pub fn signal_group(pid: i32, signal: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let pgid = unsafe { libc::getpgid(pid) };
    let rc = if pgid == pid {
        unsafe { libc::killpg(pid, signal) }
    } else {
        tracing::warn!(
            pid,
            pgid,
            "forge-cdp: процесс не лидер своей группы, сигнал шлётся только ему"
        );
        unsafe { libc::kill(pid, signal) }
    };
    rc == 0
}

/// Гасит группу: `SIGTERM`, отсрочка, `SIGKILL`.
///
/// Для ЧУЖИХ процессов (сирот от прошлого запуска devforge). Для собственного
/// потомка есть [`crate::launch::ChromeProcess::terminate`]: своего ребёнка
/// надо ещё и дождаться (`waitpid`), иначе он останется зомби и проверка
/// «жив» будет отвечать «да» вечно.
///
/// Возвращает `true`, если процесс умер.
pub async fn terminate_group(pid: i32, grace: Duration, poll: Duration) -> bool {
    if !is_alive(pid) {
        return true;
    }
    signal_group(pid, libc::SIGTERM);
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        tokio::time::sleep(poll).await;
    }
    tracing::warn!(pid, "forge-cdp: группа не умерла по SIGTERM, шлём SIGKILL");
    signal_group(pid, libc::SIGKILL);
    // SIGKILL не перехватывается, но ядру нужно время снять процесс с учёта.
    let hard_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < hard_deadline {
        if !is_alive(pid) {
            return true;
        }
        tokio::time::sleep(poll).await;
    }
    false
}

/// Настройки подметания сирот.
#[derive(Debug, Clone)]
pub struct SweepConfig {
    /// Сколько ждать после `SIGTERM` перед `SIGKILL`. Поле, а не константа:
    /// в бою это 5 секунд, в тесте — миллисекунды.
    pub term_grace: Duration,
    /// Шаг опроса «умер ли».
    pub poll_interval: Duration,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            term_grace: Duration::from_secs(5),
            poll_interval: Duration::from_millis(100),
        }
    }
}

/// Что случилось с конкретным `chrome.pid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    /// Процесс был жив, argv совпал — группа погашена `SIGTERM`.
    KilledWithTerm,
    /// То же, но пришлось эскалировать до `SIGKILL`.
    KilledWithKill,
    /// Процесс мёртв — чистим только файл.
    AlreadyDead,
    /// Процесс жив, но argv НЕ наш: pid переиспользован ОС. Не трогаем.
    ForeignPid {
        /// Первые символы чужого argv — для лога, чтобы инцидент можно было
        /// разобрать постфактум.
        argv_head: String,
    },
    /// Файл не читается.
    Unreadable(String),
}

/// Итог подметания.
#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    /// По одному элементу на каждый найденный `chrome.pid`.
    pub entries: Vec<(PathBuf, SweepOutcome)>,
}

impl SweepReport {
    /// Сколько процессов реально убито.
    pub fn killed(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, o)| {
                matches!(o, SweepOutcome::KilledWithTerm | SweepOutcome::KilledWithKill)
            })
            .count()
    }

    /// Сколько раз сработала защита от переиспользованного pid.
    pub fn foreign(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, o)| matches!(o, SweepOutcome::ForeignPid { .. }))
            .count()
    }
}

/// Обходит `<cache_root>/*/chrome.pid` и убивает наши осиротевшие браузеры.
///
/// Вызывается на старте плагина, до того как заводится первая встреча.
/// Файл удаляется в любом исходе, кроме нечитаемого: живой чужой pid означает,
/// что запись протухла и её нужно забыть, а не хранить вечно.
pub async fn sweep_orphans(cache_root: &Path, cfg: &SweepConfig) -> SweepReport {
    let mut report = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return report;
    };
    for entry in entries.flatten() {
        let pid_file = entry.path().join(PID_FILE_NAME);
        if !pid_file.is_file() {
            continue;
        }
        let outcome = sweep_one(&pid_file, cfg).await;
        if !matches!(outcome, SweepOutcome::Unreadable(_)) {
            let _ = std::fs::remove_file(&pid_file);
        }
        tracing::info!(file = %pid_file.display(), outcome = ?outcome, "forge-cdp: подметание сирот");
        report.entries.push((pid_file, outcome));
    }
    report
}

/// Обрабатывает один `chrome.pid`. Публична ради точечных тестов.
pub async fn sweep_one(pid_file: &Path, cfg: &SweepConfig) -> SweepOutcome {
    let record = match read_pid_file(pid_file) {
        Ok(r) => r,
        Err(e) => return SweepOutcome::Unreadable(e.to_string()),
    };

    // Проверка 1: жив ли вообще.
    if !is_alive(record.pid) {
        return SweepOutcome::AlreadyDead;
    }

    // Проверка 2, ОБЯЗАТЕЛЬНАЯ: это точно наш процесс, а не унаследовавший
    // номер чужой. Отсутствие argv (ps ничего не сказал) трактуется как
    // «не наш»: не знать и убивать — худшее сочетание.
    let argv = process_argv(record.pid).unwrap_or_default();
    if !argv_matches_profile(&argv, &record.user_data_dir) {
        return SweepOutcome::ForeignPid {
            argv_head: crate::protocol::truncate_for_log(&argv, 120),
        };
    }

    signal_group(record.pid, libc::SIGTERM);
    let deadline = tokio::time::Instant::now() + cfg.term_grace;
    while tokio::time::Instant::now() < deadline {
        if !is_alive(record.pid) {
            return SweepOutcome::KilledWithTerm;
        }
        tokio::time::sleep(cfg.poll_interval).await;
    }
    if !is_alive(record.pid) {
        return SweepOutcome::KilledWithTerm;
    }
    signal_group(record.pid, libc::SIGKILL);
    let hard = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < hard && is_alive(record.pid) {
        tokio::time::sleep(cfg.poll_interval).await;
    }
    SweepOutcome::KilledWithKill
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    /// Запускает процесс в СВОЕЙ группе, как это делает `launch`.
    ///
    /// `script` уходит в `sh -c`, а следом идут `fake-chrome` и наш
    /// `--user-data-dir`: они видны в `ps -o args=` и служат тем самым
    /// признаком «наш процесс», по которому работает вторая проверка.
    ///
    /// Дочерний процесс сразу отдаётся отдельному потоку с `wait()`. Это не
    /// украшение: мёртвый потомок остаётся ЗОМБИ, пока его не забрали, а для
    /// `kill(pid, 0)` зомби неотличим от живого — без жнеца тест проверял бы
    /// не то, что нужно. В бою этой проблемы нет: сироты достаются нам от
    /// прошлого процесса devforge и их забирает launchd.
    fn spawn_group(script: &str, user_data_dir: &std::path::Path) -> i32 {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(script)
            .arg("fake-chrome")
            .arg(format!("--user-data-dir={}", user_data_dir.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Своя группа процессов: ровно то, что даёт setsid в боевом запуске.
        cmd.process_group(0);
        let mut child = cmd.spawn().expect("не удалось запустить тестовый процесс");
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        // Даём sh породить потомка, иначе группа окажется из одного процесса.
        std::thread::sleep(Duration::from_millis(300));
        pid
    }

    /// Кто ещё жив в группе.
    fn group_members(pgid: i32) -> Vec<String> {
        let out = Command::new("ps")
            .args(["-o", "pid=", "-g", &pgid.to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    async fn wait_group_empty(pgid: i32) -> bool {
        for _ in 0..40 {
            if group_members(pgid).is_empty() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    fn write_record(dir: &std::path::Path, pid: i32, user_data_dir: &std::path::Path) {
        write_pid_file(
            dir,
            &PidRecord {
                pid,
                user_data_dir: user_data_dir.to_path_buf(),
                started_unix: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn pid_record_round_trips_through_the_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("meeting");
        let rec = PidRecord {
            pid: 4242,
            user_data_dir: tmp.path().join("meeting/chrome-profile"),
            started_unix: 1_770_000_000,
        };
        write_pid_file(&dir, &rec).unwrap();
        assert_eq!(read_pid_file(&dir).unwrap(), rec);
        assert_eq!(read_pid_file(&dir.join(PID_FILE_NAME)).unwrap(), rec);
        // Временный файл за собой не оставляем.
        assert!(!dir.join(format!("{PID_FILE_NAME}.tmp")).exists());
    }

    /// ГЛАВНАЯ защита: pid жив, но argv не наш — не трогаем.
    ///
    /// Именно так выглядит переиспользованный операционной системой номер, и
    /// убийство по одному лишь живому pid однажды прибило бы посторонний
    /// процесс пользователя.
    #[tokio::test]
    async fn alive_pid_with_foreign_argv_is_never_killed() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        let meeting = cache.join("meeting-1");
        let their_profile = tmp.path().join("чужой-профиль");
        let our_profile = tmp.path().join("наш-профиль");

        let pid = spawn_group("sleep 30 & wait", &their_profile);
        // В файле записан НАШ профиль, а процесс запущен с чужим.
        write_record(&meeting, pid, &our_profile);

        let report = sweep_orphans(
            &cache,
            &SweepConfig {
                term_grace: Duration::from_millis(100),
                poll_interval: Duration::from_millis(10),
            },
        )
        .await;

        assert_eq!(report.foreign(), 1);
        assert_eq!(report.killed(), 0);
        assert!(matches!(
            report.entries[0].1,
            SweepOutcome::ForeignPid { .. }
        ));
        assert!(
            is_alive(pid),
            "чужой процесс обязан остаться живым — это защита от переиспользованного pid"
        );
        // Протухшая запись при этом убрана.
        assert!(!meeting.join(PID_FILE_NAME).exists());

        signal_group(pid, libc::SIGKILL);
        assert!(wait_group_empty(pid).await);
    }

    /// Мёртвый pid: чистим файл и не считаем это ошибкой.
    #[tokio::test]
    async fn dead_pid_is_cleaned_without_error() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        let meeting = cache.join("meeting-2");

        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap(); // забираем зомби: pid действительно свободен
        write_record(&meeting, pid, &tmp.path().join("профиль"));

        let report = sweep_orphans(&cache, &SweepConfig::default()).await;
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].1, SweepOutcome::AlreadyDead);
        assert_eq!(report.killed(), 0);
        assert!(!meeting.join(PID_FILE_NAME).exists());
    }

    /// Умирает ВСЯ группа, а не только её лидер.
    #[tokio::test]
    async fn whole_group_dies_on_sigterm() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        let meeting = cache.join("meeting-3");
        let profile = tmp.path().join("профиль-3");

        let pid = spawn_group("sleep 300 & wait", &profile);
        assert!(
            group_members(pid).len() >= 2,
            "в группе должны быть лидер и его потомок, иначе проверка бессмысленна"
        );
        write_record(&meeting, pid, &profile);

        let report = sweep_orphans(
            &cache,
            &SweepConfig {
                term_grace: Duration::from_secs(3),
                poll_interval: Duration::from_millis(20),
            },
        )
        .await;

        assert_eq!(report.entries[0].1, SweepOutcome::KilledWithTerm);
        assert_eq!(report.killed(), 1);
        assert!(
            wait_group_empty(pid).await,
            "выжившие в группе: {:?} — убит только лидер",
            group_members(pid)
        );
        assert!(!meeting.join(PID_FILE_NAME).exists());
    }

    /// Группа, игнорирующая SIGTERM, добивается SIGKILL по истечении отсрочки.
    ///
    /// Отсрочка — ПОЛЕ конфига: в бою 5 секунд, здесь 200 миллисекунд.
    #[tokio::test]
    async fn group_ignoring_sigterm_is_killed_after_the_grace_period() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        let meeting = cache.join("meeting-4");
        let profile = tmp.path().join("профиль-4");

        // Игнорирование сигнала наследуется и через fork, и через exec,
        // поэтому SIGTERM не берёт ни sh, ни его потомка.
        let pid = spawn_group(r#"trap "" TERM; sleep 300 & wait"#, &profile);
        write_record(&meeting, pid, &profile);

        let started = std::time::Instant::now();
        let report = sweep_orphans(
            &cache,
            &SweepConfig {
                term_grace: Duration::from_millis(200),
                poll_interval: Duration::from_millis(20),
            },
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(report.entries[0].1, SweepOutcome::KilledWithKill);
        assert!(
            elapsed >= Duration::from_millis(200),
            "SIGKILL не имеет права идти раньше отсрочки: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "отсрочка обязана браться из конфига, а не из константы: {elapsed:?}"
        );
        assert!(
            wait_group_empty(pid).await,
            "выжившие в группе: {:?}",
            group_members(pid)
        );
    }

    /// Битый `chrome.pid` не удаляется: его надо увидеть глазами.
    #[tokio::test]
    async fn unreadable_pid_file_is_reported_and_kept() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        let meeting = cache.join("meeting-5");
        std::fs::create_dir_all(&meeting).unwrap();
        std::fs::write(meeting.join(PID_FILE_NAME), "{это не json").unwrap();

        let report = sweep_orphans(&cache, &SweepConfig::default()).await;
        assert!(matches!(
            report.entries[0].1,
            SweepOutcome::Unreadable(_)
        ));
        assert!(
            meeting.join(PID_FILE_NAME).exists(),
            "нечитаемый файл оставляем — иначе причина пропадёт вместе с уликой"
        );
    }

    /// Обход кэша: каталоги без `chrome.pid` пропускаются, отчёт по каждому.
    #[tokio::test]
    async fn sweep_walks_all_meeting_dirs() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(cache.join("без-записи")).unwrap();
        std::fs::write(cache.join("посторонний.txt"), b"x").unwrap();

        let mut dead = Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
        let dead_pid = dead.id() as i32;
        dead.wait().unwrap();
        write_record(&cache.join("m1"), dead_pid, &tmp.path().join("p1"));
        write_record(&cache.join("m2"), dead_pid, &tmp.path().join("p2"));

        let report = sweep_orphans(&cache, &SweepConfig::default()).await;
        assert_eq!(report.entries.len(), 2);
        assert!(report
            .entries
            .iter()
            .all(|(_, o)| *o == SweepOutcome::AlreadyDead));
    }

    /// Несуществующий кэш — не ошибка: первый запуск devforge выглядит так.
    #[tokio::test]
    async fn missing_cache_root_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let report = sweep_orphans(&tmp.path().join("ещё-нет"), &SweepConfig::default()).await;
        assert!(report.entries.is_empty());
    }

    #[test]
    fn argv_match_requires_the_exact_profile() {
        let dir = std::path::Path::new("/tmp/forge/meeting-1/chrome-profile");
        assert!(argv_matches_profile(
            "Google Chrome --user-data-dir=/tmp/forge/meeting-1/chrome-profile --mute-audio",
            dir
        ));
        // Другая встреча — другой профиль: подметальщик первой не имеет права
        // убить браузер второй.
        assert!(!argv_matches_profile(
            "Google Chrome --user-data-dir=/tmp/forge/meeting-2/chrome-profile",
            dir
        ));
        assert!(!argv_matches_profile("", dir));
        assert!(!argv_matches_profile("что угодно", std::path::Path::new("")));
    }

    #[test]
    fn is_alive_says_no_about_impossible_pids() {
        assert!(!is_alive(0));
        assert!(!is_alive(-1));
        // Свой процесс жив по определению.
        assert!(is_alive(std::process::id() as i32));
    }
}
