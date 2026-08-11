//! MTProto-рантайм плагина: владение соединением, сессией и раннером.
//!
//! Здесь собирается всё, что нужно для работы с Telegram по MTProto, и
//! ровно в одном месте — чтобы «подключиться» и «отключиться» были
//! атомарными операциями над одним объектом [`TgRuntime`].
//!
//! ## Из чего состоит подключение (grammers 0.10)
//!
//! ```text
//!   FileSession::open(session_path)            // персист auth-key на диске
//!        │
//!        ├──▶ SenderPool::new(Arc<session>, api_id)
//!        │        ├── runner  ── tokio::spawn(runner.run())   // I/O-цикл
//!        │        ├── handle  ── Client::new(handle)          // RPC-фасад
//!        │        └── updates ── mpsc::UnboundedReceiver      // сырые апдейты
//!        └──▶ flock(session_path)                              // риск №5
//! ```
//!
//! `SenderPoolRunner::run()` — единственная задача, которая реально
//! разговаривает с сетью; `Client` — дешёвый клон-фасад поверх канала к
//! ней. Поэтому клиент можно клонировать сколько угодно, а раннер живёт
//! ровно один и абортится при [`TgRuntime::close`] / `Drop`.
//!
//! ## Одна сессия — один процесс (риск №5 плана)
//!
//! Файл сессии содержит auth-key аккаунта. Два процесса, одновременно
//! пишущих в него, ломают MTProto-состояние (Telegram начинает отвечать
//! `AUTH_KEY_DUPLICATED` и разлогинивает устройство). Поэтому
//! [`build_runtime`] берёт эксклюзивный неблокирующий лок через
//! `std::fs::File::try_lock` (стабилизировано в Rust 1.89 — внешний крейт
//! не нужен) и держит его столько же, сколько живёт [`TgRuntime`].
//!
//! Лок берётся на файл-компаньон `<session_path>.lock`, а НЕ на саму
//! сессию: [`crate::session::FileSession`] перезаписывает файл сессии
//! через `rename`, то есть заменяет inode, и лок, взятый на прежний
//! дескриптор, перестал бы что-либо защищать. Отдельный lock-файл живёт
//! на своём inode и даёт ту же гарантию «одна сессия — один процесс».
//!
//! ## Никаких авто-ретраев на логине (риск №2 плана)
//!
//! [`flood_wait_secs`] только ИЗВЛЕКАЕТ количество секунд из ошибки
//! `FLOOD_WAIT_X`. Решение «повторить» принимает исключительно
//! пользователь: FLOOD_WAIT на шагах логина измеряется часами, а каждая
//! автоматическая попытка продлевает блокировку.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use grammers_client::Client;
use grammers_mtsender::{InvocationError, SenderPool};
use grammers_session::updates::UpdatesLike;

use crate::session::FileSession;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::config::TelegramConfig;

/// Сколько ждать ответа Telegram на проверку авторизации при старте.
///
/// Сеть может висеть (капризный Wi-Fi, DPI, отсутствие интернета), а
/// старт сервера ждать не должен — по истечении таймаута плагин просто
/// уходит в `Disconnected{error}` и остаётся живым.
pub const AUTH_CHECK_TIMEOUT_SECS: u64 = 10;

/// Сколько ждать корректного завершения раннера при закрытии рантайма,
/// прежде чем оборвать его `abort()`.
const RUNNER_STOP_GRACE_SECS: u64 = 2;

/// Эксклюзивный лок MTProto-сессии («одна сессия — один процесс»).
///
/// Живёт ровно столько же, сколько [`TgRuntime`]: при `Drop` дескриптор
/// закрывается и лок снимается ядром автоматически (`unlock` вызывается
/// явно только ради читаемости).
#[derive(Debug)]
pub struct SessionLock {
    session_path: PathBuf,
    lock_path: PathBuf,
    file: File,
}

impl SessionLock {
    /// Путь lock-файла для данной сессии: `<session_path>.lock`.
    ///
    /// Отдельный файл, а не сама сессия — см. «Одна сессия — один
    /// процесс» в шапке модуля (flock на базе ломает libsql).
    pub fn lock_path_for(session_path: &Path) -> PathBuf {
        let mut name = session_path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        session_path.with_file_name(name)
    }

    /// Берёт неблокирующий эксклюзивный лок сессии `session_path`,
    /// создавая lock-файл (и родительский каталог) при необходимости.
    ///
    /// # Errors
    ///
    /// - лок занят другим процессом — сообщение прямо называет причину и
    ///   путь, чтобы пользователь понял, что запущен второй devforge;
    /// - файл не открывается (нет каталога, нет прав) — io-ошибка с путём.
    pub fn acquire(session_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = session_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("failed to create {}: {e}", parent.display())
                })?;
            }
        }
        let lock_path = Self::lock_path_for(session_path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", lock_path.display()))?;

        match file.try_lock() {
            Ok(()) => Ok(Self {
                session_path: session_path.to_path_buf(),
                lock_path,
                file,
            }),
            Err(TryLockError::WouldBlock) => Err(anyhow::anyhow!(
                "Telegram-сессия уже используется другим процессом devforge (файл {}). \
                 Одна сессия — один процесс: остановите второй экземпляр или укажите \
                 другой session_path в ~/.config/forge/telegram.toml",
                session_path.display()
            )),
            Err(TryLockError::Error(e)) => Err(anyhow::anyhow!(
                "failed to lock Telegram session file {}: {e}",
                lock_path.display()
            )),
        }
    }

    /// Путь сессии, которую защищает лок.
    pub fn path(&self) -> &Path {
        &self.session_path
    }

    /// Путь самого lock-файла (для логов и тестов).
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Активное MTProto-подключение.
///
/// Владеет всем, что нельзя расшарить: раннером, приёмником апдейтов и
/// локом сессии. `client` при этом дешёво клонируется — хендлеры берут
/// клон под `RwLock`-read и работают с ним уже без удержания лока.
pub struct TgRuntime {
    /// RPC-фасад grammers. `Clone` — дешёвый (Arc внутри).
    pub client: Client,
    /// Хранилище сессии; нужно для `SenderPool` и резолва peer'ов.
    pub session: Arc<FileSession>,
    /// Задача `SenderPoolRunner::run()` — единственный сетевой цикл.
    pub runner_handle: JoinHandle<()>,
    /// Приёмник сырых апдейтов. `Option`, потому что
    /// `Client::stream_updates` ПОГЛОЩАЕТ его: updates-воркер Фазы 3
    /// заберёт значение через [`TgRuntime::take_updates`] ровно один раз.
    pub updates_rx: Option<UnboundedReceiver<UpdatesLike>>,
    /// Unix-время установления соединения (секунды).
    pub connected_at: i64,
    /// Лок файла сессии; освобождается вместе с рантаймом.
    lock: SessionLock,
}

impl std::fmt::Debug for TgRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TgRuntime")
            .field("session_path", &self.lock.path())
            .field("connected_at", &self.connected_at)
            .field("updates_taken", &self.updates_rx.is_none())
            .finish()
    }
}

impl TgRuntime {
    /// Забирает приёмник апдейтов (одноразово).
    ///
    /// Второй вызов вернёт `None` — это защита от двух параллельных
    /// updates-воркеров на одном соединении.
    pub fn take_updates(&mut self) -> Option<UnboundedReceiver<UpdatesLike>> {
        self.updates_rx.take()
    }

    /// Путь файла сессии, на который взят лок.
    pub fn session_path(&self) -> &Path {
        self.lock.path()
    }

    /// Корректно закрывает подключение.
    ///
    /// Сначала просит пул завершиться штатно (`Client::disconnect` шлёт
    /// `Quit` раннеру), ждёт [`RUNNER_STOP_GRACE_SECS`] и только потом
    /// абортит задачу. Так процесс не зависает на выходе, но и не рвёт
    /// соединение в середине записи сессии.
    pub async fn close(&mut self) {
        // Сначала дожимаем отложенное троттлингом состояние сессии: pts и
        // кэш peer'ов, которые ещё не попали на диск (auth-key пишется
        // немедленно и здесь уже сохранён).
        if let Err(e) = self.session.flush_now() {
            tracing::warn!(
                target: "forge_telegram",
                error = %e,
                "forge-telegram: could not flush session state on close"
            );
        }
        self.client.disconnect();
        let stop = tokio::time::timeout(
            Duration::from_secs(RUNNER_STOP_GRACE_SECS),
            &mut self.runner_handle,
        )
        .await;
        if stop.is_err() {
            tracing::debug!(
                target: "forge_telegram",
                "forge-telegram: MTProto runner did not stop in time; aborting"
            );
            self.runner_handle.abort();
        }
    }
}

impl Drop for TgRuntime {
    fn drop(&mut self) {
        // Синхронный путь: если рантайм уронили без close(), раннер не
        // должен пережить его и продолжать держать сокеты.
        self.client.disconnect();
        self.runner_handle.abort();
    }
}

/// Поднимает MTProto-подключение по конфигу.
///
/// Сеть здесь ещё не трогается: `SenderPoolRunner` подключается к DC
/// лениво, на первом же RPC. Поэтому функция быстрая и её безопасно
/// вызывать из хендлера логина.
///
/// # Errors
///
/// - нет `api_id`/`api_hash` — логин невозможен, честная ошибка вместо паники;
/// - файл сессии занят другим процессом (см. [`SessionLock::acquire`]);
/// - SQLite-сессия не открывается (битый файл, нет прав).
pub async fn build_runtime(cfg: &TelegramConfig) -> anyhow::Result<TgRuntime> {
    let api_id = cfg.api_id.ok_or_else(|| {
        anyhow::anyhow!("api_id is not configured — save application credentials first")
    })?;
    if !cfg.has_credentials() {
        anyhow::bail!("api_hash is not configured — save application credentials first");
    }

    let lock = SessionLock::acquire(&cfg.session_path)?;

    let session = FileSession::open(&cfg.session_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to open Telegram session at {}: {e}",
            cfg.session_path.display()
        )
    })?;
    let session = Arc::new(session);

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(Arc::clone(&session), api_id);

    let client = Client::new(handle);
    let runner_handle = tokio::spawn(runner.run());

    tracing::info!(
        target: "forge_telegram",
        session_path = %cfg.session_path.display(),
        "forge-telegram: MTProto runtime started"
    );

    Ok(TgRuntime {
        client,
        session,
        runner_handle,
        updates_rx: Some(updates),
        connected_at: chrono::Utc::now().timestamp(),
        lock,
    })
}

/// Достаёт `X` из ошибки `FLOOD_WAIT_X` (RPC-код 420).
///
/// Возвращает `None` для любой другой ошибки — вызывающий код тогда
/// маппит её в `503 not_connected` / `400`, а не в `429`.
///
/// НИКОГДА не используется для автоматического повтора: значение уходит
/// в тело `429 {retry_after_secs}`, и повтор инициирует пользователь.
pub fn flood_wait_secs(err: &InvocationError) -> Option<u32> {
    match err {
        // 420 — семейство «подожди»: FLOOD_WAIT, FLOOD_PREMIUM_WAIT,
        // SLOWMODE_WAIT, TAKEOUT_INIT_DELAY. Все несут секунды в `value`.
        InvocationError::Rpc(rpc) if rpc.code == 420 => rpc.value,
        // Подстраховка: тот же смысл, но сервер прислал другой код.
        InvocationError::Rpc(rpc) if rpc.name.contains("FLOOD_WAIT") => rpc.value,
        _ => None,
    }
}

/// Проверка авторизации с таймаутом.
///
/// Обычный `client.is_authorized()` может висеть столько, сколько висит
/// сеть. Восстановление сессии при старте обязано укладываться в
/// предсказуемое время, иначе плагин задержит поднятие сервера.
///
/// # Errors
///
/// Ошибка RPC или истёкший таймаут; в обоих случаях текст пригоден для
/// показа в `Disconnected{error}`.
pub async fn is_authorized_with_timeout(client: &Client, dur: Duration) -> anyhow::Result<bool> {
    match tokio::time::timeout(dur, client.is_authorized()).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "failed to query Telegram authorization state: {e}"
        )),
        Err(_) => Err(anyhow::anyhow!(
            "Telegram did not respond within {}s while checking authorization",
            dur.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_tl_types as tl;

    fn rpc(code: i32, message: &str) -> InvocationError {
        InvocationError::Rpc(grammers_mtsender::RpcError::from(tl::types::RpcError {
            error_code: code,
            error_message: message.to_string(),
        }))
    }

    #[test]
    fn flood_wait_secs_extracts_seconds() {
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_31")), Some(31));
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_86400")), Some(86400));
        assert_eq!(flood_wait_secs(&rpc(420, "SLOWMODE_WAIT_7")), Some(7));
    }

    #[test]
    fn flood_wait_secs_ignores_other_errors() {
        assert_eq!(flood_wait_secs(&rpc(400, "PHONE_CODE_INVALID")), None);
        assert_eq!(flood_wait_secs(&rpc(401, "AUTH_KEY_UNREGISTERED")), None);
        assert_eq!(flood_wait_secs(&rpc(500, "INTERNAL")), None);
        assert_eq!(flood_wait_secs(&InvocationError::Dropped), None);
    }

    #[test]
    fn flood_wait_secs_handles_malformed_and_extreme_values() {
        // Значение отсутствует или это не число — «подожди N секунд»
        // извлечь нельзя, честный None вместо паники или мусора.
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_")), None);
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_X")), None);
        // Переполнение u32: grammers не парсит такое значение, ошибка
        // деградирует в None (её классифицируют как not_connected).
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_99999999999")), None);
        // Граничные значения, которые сервер реально может прислать.
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_WAIT_0")), Some(0));
        assert_eq!(
            flood_wait_secs(&rpc(420, "FLOOD_WAIT_4294967295")),
            Some(u32::MAX)
        );
        // Родственник из того же семейства 420 несёт секунды так же.
        assert_eq!(flood_wait_secs(&rpc(420, "FLOOD_PREMIUM_WAIT_17")), Some(17));
    }

    #[test]
    fn flood_wait_secs_matches_name_on_unexpected_code() {
        // Подстраховочная ветка: смысл FLOOD_WAIT, но код не 420 —
        // Telegram изредка шлёт flood-ошибки с кодом 400/500.
        assert_eq!(flood_wait_secs(&rpc(400, "FLOOD_WAIT_5")), Some(5));
        assert_eq!(flood_wait_secs(&rpc(500, "FLOOD_WAIT_60")), Some(60));
        // PEER_FLOOD — другая ошибка (спам-бан без срока): секунды из неё
        // выдумывать нельзя.
        assert_eq!(flood_wait_secs(&rpc(400, "PEER_FLOOD")), None);
    }

    #[test]
    fn lock_path_for_handles_unusual_session_names() {
        // Имя без расширения: .lock просто добавляется.
        assert_eq!(
            SessionLock::lock_path_for(Path::new("/x/tg")),
            PathBuf::from("/x/tg.lock")
        );
        // Несколько точек: .lock ДОБАВЛЯЕТСЯ, а не заменяет «расширение» —
        // иначе сессии a.b.session и a.b.other делили бы один lock-файл.
        assert_eq!(
            SessionLock::lock_path_for(Path::new("/x/a.b.session")),
            PathBuf::from("/x/a.b.session.lock")
        );
        // Скрытый файл.
        assert_eq!(
            SessionLock::lock_path_for(Path::new("/x/.session")),
            PathBuf::from("/x/.session.lock")
        );
        // Путь без имени файла не должен паниковать.
        assert_eq!(
            SessionLock::lock_path_for(Path::new("/")),
            PathBuf::from("/.lock")
        );
    }

    #[test]
    fn session_lock_works_with_unicode_paths() {
        // Домашние каталоги с кириллицей/эмодзи — реальность macOS.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("тг 📱.session");

        let lock = SessionLock::acquire(&path).unwrap();
        assert!(lock.lock_path().exists());
        assert!(
            SessionLock::acquire(&path).is_err(),
            "эксклюзивность не должна зависеть от имени файла"
        );
    }

    #[test]
    fn stale_garbage_in_lock_file_does_not_break_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let lock_path = SessionLock::lock_path_for(&path);
        // «Наследство» упавшего процесса: мусорные байты в lock-файле.
        // flock снимается ядром при смерти процесса, содержимое остаётся.
        std::fs::write(&lock_path, b"\x00\xffgarbage from crashed process").unwrap();

        let lock = SessionLock::acquire(&path).unwrap();
        // Лок берётся flock'ом: файл не трункейтится и не переписывается.
        assert_eq!(
            std::fs::read(lock.lock_path()).unwrap(),
            b"\x00\xffgarbage from crashed process"
        );
        drop(lock);
        let _again = SessionLock::acquire(&path).unwrap();
    }

    #[test]
    fn acquire_reports_unusable_parent_directory() {
        // Пользователь указал в telegram.toml путь, «каталог» которого —
        // обычный файл: ошибка обязана назвать шаг и путь.
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("actually-a-file");
        std::fs::write(&not_a_dir, b"i am a file").unwrap();

        let err = SessionLock::acquire(&not_a_dir.join("telegram.session"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to create"), "{err}");
        assert!(err.contains("actually-a-file"), "{err}");
    }

    #[test]
    fn acquire_reports_lock_path_occupied_by_directory() {
        // На месте lock-файла оказался каталог (следы чужого tooling'а):
        // открыть его на запись нельзя, ошибка должна назвать путь.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        std::fs::create_dir(SessionLock::lock_path_for(&path)).unwrap();

        let err = SessionLock::acquire(&path).unwrap_err().to_string();
        assert!(err.contains("failed to open"), "{err}");
        assert!(err.contains("telegram.session.lock"), "{err}");
    }

    #[test]
    fn concurrent_acquire_admits_exactly_one() {
        // Гонка старта двух devforge: сколько бы потоков ни рвануло к
        // сессии одновременно, лок достаётся ровно одному.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");

        let n = 8;
        let start = std::sync::Arc::new(std::sync::Barrier::new(n));
        let hold = std::sync::Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let path = path.clone();
                let start = std::sync::Arc::clone(&start);
                let hold = std::sync::Arc::clone(&hold);
                std::thread::spawn(move || {
                    start.wait();
                    let res = SessionLock::acquire(&path);
                    let ok = res.is_ok();
                    // Победитель держит лок, пока ВСЕ не попробовали, —
                    // иначе успехи могли бы выстроиться последовательно.
                    hold.wait();
                    ok
                })
            })
            .collect();

        let oks = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|&ok| ok)
            .count();
        assert_eq!(oks, 1, "лок обязан достаться ровно одному потоку");
    }

    #[test]
    fn session_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/telegram.session");

        let first = SessionLock::acquire(&path).unwrap();
        assert_eq!(first.path(), path.as_path());
        assert_eq!(first.lock_path(), path.with_extension("session.lock"));
        assert!(first.lock_path().exists(), "lock-файл создан");

        // Второй лок в этом же процессе тоже обязан быть отвергнут:
        // flock работает по описанию открытого файла, а не по процессу.
        let err = SessionLock::acquire(&path).unwrap_err().to_string();
        assert!(
            err.contains("уже используется другим процессом"),
            "сообщение должно объяснять причину: {err}"
        );

        drop(first);
        // После освобождения лок снова берётся.
        let _second = SessionLock::acquire(&path).unwrap();
    }

    #[test]
    fn session_lock_never_touches_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        std::fs::write(&path, b"SQLite format 3\0").unwrap();

        let lock = SessionLock::acquire(&path).unwrap();
        // Лок живёт в отдельном файле и не трогает саму базу — иначе
        // libsql отвечает «database is locked» (см. шапку модуля).
        assert_eq!(std::fs::read(&path).unwrap(), b"SQLite format 3\0");
        assert_ne!(lock.lock_path(), path.as_path());
        drop(lock);
    }

    #[tokio::test]
    async fn build_runtime_fails_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let err = build_runtime(&cfg).await.unwrap_err().to_string();
        assert!(err.contains("api_id"), "got: {err}");

        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: None,
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let err = build_runtime(&cfg).await.unwrap_err().to_string();
        assert!(err.contains("api_hash"), "got: {err}");
    }

    #[tokio::test]
    async fn build_runtime_reports_busy_session() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        // Имитируем «второй процесс»: лок уже занят.
        let _held = SessionLock::acquire(&cfg.session_path).unwrap();
        let err = build_runtime(&cfg).await.unwrap_err().to_string();
        assert!(
            err.contains("уже используется другим процессом"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn build_runtime_starts_and_closes_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        // Соединение ленивое: build_runtime не ходит в сеть.
        let mut rt = build_runtime(&cfg).await.unwrap();
        assert!(rt.take_updates().is_some());
        assert!(rt.take_updates().is_none(), "апдейты забираются один раз");
        assert_eq!(rt.session_path(), cfg.session_path.as_path());

        rt.close().await;
        drop(rt);

        // Лок отпущен вместе с рантаймом — сессию можно взять снова.
        let _again = SessionLock::acquire(&cfg.session_path).unwrap();
    }

    #[tokio::test]
    async fn build_runtime_releases_lock_when_session_cannot_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        // Путь сессии занят каталогом: read() падает НЕ NotFound'ом, и
        // FileSession::open честно возвращает ошибку (битый JSON — это
        // «новая сессия», а вот I/O-ошибка — нет).
        std::fs::create_dir(&cfg.session_path).unwrap();

        let err = build_runtime(&cfg).await.unwrap_err().to_string();
        assert!(err.contains("failed to open Telegram session"), "got: {err}");

        // Лок, взятый ДО неудачного открытия сессии, не должен утечь:
        // иначе повторная попытка (пользователь починил конфиг) навсегда
        // упиралась бы в «уже используется другим процессом».
        let _retry = SessionLock::acquire(&cfg.session_path).unwrap();
    }

    #[tokio::test]
    async fn dropping_runtime_without_close_releases_lock() {
        // Аварийный путь: рантайм уронили без close() (паника хендлера,
        // ранний return). Drop обязан снять лок — иначе до конца жизни
        // процесса сессию нельзя переподключить.
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let rt = build_runtime(&cfg).await.unwrap();
        drop(rt);

        let _again = SessionLock::acquire(&cfg.session_path).unwrap();
    }

    #[tokio::test]
    async fn is_authorized_with_timeout_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            api_id: Some(1),
            api_hash: Some("hash".into()),
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let mut rt = build_runtime(&cfg).await.unwrap();
        // Сети в тестах нет: либо мгновенная ошибка соединения, либо
        // таймаут. Оба исхода — Err, и оба несут читаемый текст.
        let err = is_authorized_with_timeout(&rt.client, Duration::from_millis(150))
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.is_empty());
        rt.close().await;
    }
}
