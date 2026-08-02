//! Хранилище MTProto-сессии на файле (реализация `grammers_session::Session`).
//!
//! # Почему НЕ `SqliteSession` (план Б из плана, риск №1)
//!
//! `grammers-session` со фичей `sqlite-storage` тянет `libsql`, который
//! статически линкует СВОЙ SQLite. В том же бинаре уже есть SQLite от
//! `rusqlite` (features `bundled`) — им пользуется плагин Echo. Оба
//! экспортируют символы `sqlite3_*`, линковщик оставляет одну реализацию,
//! и получается фатальная гонка порядка инициализации:
//!
//! 1. Echo открывает `echo.db` на старте → `sqlite3_initialize()`;
//! 2. позже плагин Telegram открывает сессию → `libsql` вызывает
//!    `sqlite3_config(SQLITE_CONFIG_SERIALIZED)`;
//! 3. `sqlite3_config` после `initialize` возвращает `SQLITE_MISUSE` (21),
//!    а `libsql` проверяет это через `assert_eq!` ⇒ **паника** в
//!    `libsql-0.9.30/src/local/database.rs:323`.
//!
//! Это воспроизводится в реальном `devforge start --telegram` на первом же
//! `POST /login/start`: хендлер падает, соединение обрывается (HTTP 000).
//! Обойти порядок инициализации нельзя — Echo всегда стартует раньше.
//!
//! Поэтому реализован план Б, прямо предусмотренный планом: `Session` —
//! это trait, и плагин хранит сессию сам. libsql из дерева убран
//! (`grammers-session` подключается с `default-features = false, features =
//! ["serde"]`), двойного SQLite больше нет.
//!
//! # Формат и безопасность
//!
//! Состояние сериализуется в JSON ([`PersistedSession`], поле `version` для
//! будущих миграций) и пишется АТОМАРНО (tmp + `fsync` + `rename`) с правами
//! `0600` — в нём лежит `auth_key` аккаунта, фактический ключ от переписки.
//!
//! # Троттлинг записи
//!
//! `cache_peer` и `set_update_state` вызываются на каждом апдейте, поэтому
//! они помечают состояние «грязным» и пишут файл не чаще
//! [`FLUSH_THROTTLE`]. А вот `set_home_dc_id` / `set_dc_option` пишут
//! НЕМЕДЛЕННО: там появляется auth-key, и потерять его при падении процесса
//! значило бы потребовать от пользователя повторный вход (а это FLOOD_WAIT).
//! [`FileSession::flush_now`] дожимает отложенное на shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use serde::{Deserialize, Serialize};

/// Как часто разрешено писать файл при «дешёвых» мутациях (peers, pts).
const FLUSH_THROTTLE: Duration = Duration::from_secs(2);

/// Версия формата файла сессии.
const FORMAT_VERSION: u32 = 1;

/// Сериализуемый снапшот сессии.
///
/// `SessionData` из grammers не выводит serde, поэтому у нас собственная
/// плоская проекция. `peers` — вектор, а не map: `PeerId` плохой ключ для
/// JSON-объекта, а `PeerInfo` и так несёт свой id.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedSession {
    /// Версия формата (см. [`FORMAT_VERSION`]).
    version: u32,
    /// Домашний DC пользователя.
    home_dc: i32,
    /// Известные DC вместе с auth-key'ами.
    dc_options: Vec<DcOption>,
    /// Кэш peer'ов (в т.ч. запись self-user — по ней grammers понимает,
    /// что аккаунт вошёл).
    peers: Vec<PeerInfo>,
    /// Состояние апдейтов (pts/qts/date/seq + каналы).
    updates_state: UpdatesState,
}

/// Ошибка файлового хранилища сессии.
///
/// `Send + Sync + 'static` обязательны: `SenderPool::new` требует этого от
/// `Session::Error`.
#[derive(Debug)]
pub enum FileSessionError {
    /// Mutex состояния отравлен паникой другого потока.
    Poisoned,
    /// Ошибка ввода-вывода при чтении/записи файла.
    Io(std::io::Error),
    /// Не удалось (де)сериализовать JSON.
    Serde(serde_json::Error),
}

impl std::fmt::Display for FileSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Poisoned => f.write_str("session lock is poisoned"),
            Self::Io(e) => write!(f, "session file I/O error: {e}"),
            Self::Serde(e) => write!(f, "session file is not valid JSON: {e}"),
        }
    }
}

impl std::error::Error for FileSessionError {}

impl From<std::io::Error> for FileSessionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for FileSessionError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
    }
}

/// Изменяемая часть хранилища под одним мьютексом.
struct Inner {
    data: SessionData,
    /// Есть несохранённые «дешёвые» изменения.
    dirty: bool,
    /// Когда последний раз писали файл.
    last_flush: Instant,
}

/// Сессия MTProto, персистящаяся в один JSON-файл.
pub struct FileSession {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for FileSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSession")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileSession {
    /// Открывает (или создаёт) сессию по пути `path`.
    ///
    /// Отсутствие файла — штатный случай «новая сессия». БИТЫЙ файл тоже не
    /// фатален: он логируется warn'ом и трактуется как новая сессия.
    /// Обоснование: восстановить auth-key из повреждённого JSON всё равно
    /// нельзя, а падать на старте из-за этого нельзя тем более — иначе
    /// пользователь не сможет даже войти заново.
    ///
    /// # Errors
    ///
    /// Только I/O-ошибки, отличные от `NotFound` (нет прав на чтение и т.п.).
    pub fn open(path: &Path) -> Result<Self, FileSessionError> {
        let data = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedSession>(&bytes) {
                Ok(p) => {
                    tracing::info!(
                        target: "forge_telegram",
                        path = %path.display(),
                        peers = p.peers.len(),
                        dcs = p.dc_options.len(),
                        "forge-telegram: loaded MTProto session from disk"
                    );
                    p.into_session_data()
                }
                Err(e) => {
                    tracing::warn!(
                        target: "forge_telegram",
                        path = %path.display(),
                        error = %e,
                        "forge-telegram: session file is corrupt; starting a fresh session \
                         (a new login will be required)"
                    );
                    SessionData::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    target: "forge_telegram",
                    path = %path.display(),
                    "forge-telegram: no session file yet, starting fresh"
                );
                SessionData::default()
            }
            Err(e) => return Err(FileSessionError::Io(e)),
        };

        Ok(Self {
            path: path.to_path_buf(),
            inner: Mutex::new(Inner {
                data,
                dirty: false,
                // Первая «дешёвая» мутация сразу имеет право на запись.
                last_flush: Instant::now() - FLUSH_THROTTLE,
            }),
        })
    }

    /// Путь файла сессии.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Немедленно сбрасывает несохранённые изменения на диск.
    ///
    /// Вызывается при закрытии рантайма: то, что отложил троттлинг, не
    /// должно теряться при штатной остановке.
    ///
    /// # Errors
    ///
    /// Ошибка записи файла.
    pub fn flush_now(&self) -> Result<(), FileSessionError> {
        let mut guard = self.inner.lock().map_err(|_| FileSessionError::Poisoned)?;
        if !guard.dirty {
            return Ok(());
        }
        self.write_locked(&mut guard)
    }

    /// Пишет файл и снимает флаг `dirty` (вызывать под уже взятым lock'ом).
    fn write_locked(&self, guard: &mut Inner) -> Result<(), FileSessionError> {
        let payload = PersistedSession::from_session_data(&guard.data);
        let bytes = serde_json::to_vec_pretty(&payload)?;

        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = crate::config::tmp_path_for(&self.path);
        // Тот же примитив, что и для telegram.toml: 0600 в момент создания,
        // fsync, атомарный rename.
        let write = crate::config::write_secret_file(&tmp, &bytes)
            .and_then(|()| std::fs::rename(&tmp, &self.path).map_err(anyhow::Error::from))
            .and_then(|()| crate::config::set_owner_only(&self.path));
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(FileSessionError::Io(std::io::Error::other(e.to_string())));
        }

        guard.dirty = false;
        guard.last_flush = Instant::now();
        Ok(())
    }

    /// Запись «критичного» изменения — всегда сразу (там auth-key).
    fn flush_critical(&self, guard: &mut Inner) -> Result<(), FileSessionError> {
        guard.dirty = true;
        self.write_locked(guard)
    }

    /// Запись «дешёвого» изменения — не чаще [`FLUSH_THROTTLE`].
    fn flush_throttled(&self, guard: &mut Inner) -> Result<(), FileSessionError> {
        guard.dirty = true;
        if guard.last_flush.elapsed() < FLUSH_THROTTLE {
            return Ok(());
        }
        self.write_locked(guard)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, FileSessionError> {
        self.inner.lock().map_err(|_| FileSessionError::Poisoned)
    }
}

impl PersistedSession {
    fn from_session_data(data: &SessionData) -> Self {
        Self {
            version: FORMAT_VERSION,
            home_dc: data.home_dc,
            dc_options: data.dc_options.values().cloned().collect(),
            peers: data.peer_infos.values().cloned().collect(),
            updates_state: data.updates_state.clone(),
        }
    }

    fn into_session_data(self) -> SessionData {
        // Стартуем с дефолтов: там лежат статически известные DC, которые
        // trait обязан отдавать даже без персиста. Персист накладывается
        // сверху и перекрывает совпадающие id (в них есть auth-key).
        let defaults = SessionData::default();
        let mut dc_options = defaults.dc_options;
        for dc in self.dc_options {
            dc_options.insert(dc.id, dc);
        }
        let mut peer_infos: HashMap<PeerId, PeerInfo> = HashMap::with_capacity(self.peers.len());
        for peer in self.peers {
            peer_infos.insert(peer.id(), peer);
        }
        SessionData {
            home_dc: self.home_dc,
            dc_options,
            peer_infos,
            updates_state: self.updates_state,
        }
    }
}

impl Session for FileSession {
    type Error = FileSessionError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Ok(self.lock()?.data.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            let mut guard = self.lock()?;
            if guard.data.home_dc == dc_id {
                return Ok(());
            }
            guard.data.home_dc = dc_id;
            self.flush_critical(&mut guard)
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        Ok(self.lock()?.data.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let mut guard = self.lock()?;
            if guard.data.dc_options.get(&dc_option.id) == Some(&dc_option) {
                return Ok(());
            }
            guard.data.dc_options.insert(dc_option.id, dc_option);
            // Здесь появляется auth-key — пишем немедленно.
            self.flush_critical(&mut guard)
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move { Ok(self.lock()?.data.peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        let peer = peer.clone();
        Box::pin(async move {
            let mut guard = self.lock()?;
            let changed = match guard.data.peer_infos.get_mut(&peer.id()) {
                Some(existing) => existing.extend_info(&peer),
                None => {
                    guard.data.peer_infos.insert(peer.id(), peer);
                    true
                }
            };
            if !changed {
                return Ok(());
            }
            self.flush_throttled(&mut guard)
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move { Ok(self.lock()?.data.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            let mut guard = self.lock()?;
            let state = &mut guard.data.updates_state;
            match update {
                UpdateState::All(updates_state) => *state = updates_state,
                UpdateState::Primary { pts, date, seq } => {
                    state.pts = pts;
                    state.date = date;
                    state.seq = seq;
                }
                UpdateState::Secondary { qts } => state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    state.channels.retain(|c| c.id != id);
                    state.channels.push(ChannelState { id, pts });
                }
            }
            self.flush_throttled(&mut guard)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dc(id: i32, key: Option<[u8; 256]>) -> DcOption {
        DcOption {
            id,
            ipv4: "149.154.167.51:443".parse().unwrap(),
            ipv6: "[2001:67c:4e8:f002::a]:443".parse().unwrap(),
            auth_key: key,
        }
    }

    #[tokio::test]
    async fn fresh_session_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let s = FileSession::open(&path).unwrap();

        // Файл ещё не создан — сессия новая, но статически известные DC есть.
        assert!(!path.exists());
        assert!(s.home_dc_id().unwrap() > 0);
        assert!(
            s.dc_option(s.home_dc_id().unwrap()).unwrap().is_some(),
            "статически известный DC обязан отдаваться без персиста"
        );
        assert!(s.peer(PeerId::self_user()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn auth_key_is_persisted_immediately_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");

        {
            let s = FileSession::open(&path).unwrap();
            s.set_dc_option(&dc(2, Some([7u8; 256]))).await.unwrap();
            s.set_home_dc_id(2).await.unwrap();
            // Никаких flush_now: auth-key пишется сразу.
            assert!(path.exists(), "файл сессии обязан появиться немедленно");
        }

        let s = FileSession::open(&path).unwrap();
        assert_eq!(s.home_dc_id().unwrap(), 2);
        let opt = s.dc_option(2).unwrap().unwrap();
        assert_eq!(opt.auth_key, Some([7u8; 256]));
    }

    #[tokio::test]
    async fn session_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/telegram.session");
        let s = FileSession::open(&path).unwrap();
        s.set_dc_option(&dc(2, Some([1u8; 256]))).await.unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "в файле auth-key, ожидались права 0600: {mode:o}");
        }
        // Временных файлов не осталось.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "остались tmp-файлы: {leftovers:?}");
    }

    #[tokio::test]
    async fn peers_and_update_state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");

        {
            let s = FileSession::open(&path).unwrap();
            // Критичная мутация создаёт файл...
            s.set_dc_option(&dc(2, Some([3u8; 256]))).await.unwrap();
            s.cache_peer(&PeerInfo::User {
                id: 777,
                auth: None,
                is_self: Some(true),
                bot: Some(false),
            })
            .await
            .unwrap();
            s.set_update_state(UpdateState::Primary {
                pts: 111,
                date: 222,
                seq: 333,
            })
            .await
            .unwrap();
            // ...а отложенные троттлингом изменения дожимает flush_now.
            s.flush_now().unwrap();
        }

        let s = FileSession::open(&path).unwrap();
        let self_peer = s.peer(PeerId::user(777).unwrap()).await.unwrap();
        assert!(self_peer.is_some(), "self-user обязан переживать рестарт");
        let st = s.updates_state().await.unwrap();
        assert_eq!((st.pts, st.date, st.seq), (111, 222, 333));
    }

    #[tokio::test]
    async fn corrupt_file_starts_fresh_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        std::fs::write(&path, b"{not json at all").unwrap();

        // Битый файл не должен ронять старт: логин можно будет пройти заново.
        let s = FileSession::open(&path).unwrap();
        assert!(s.peer(PeerId::self_user()).await.unwrap().is_none());
        assert!(s.dc_option(s.home_dc_id().unwrap()).unwrap().is_some());
    }

    /// Обрыв записи/копирования: пустой файл и валидный JSON, обрезанный на
    /// середине. Оба — «новая сессия» (восстанавливать нечего), а первая же
    /// критичная запись чинит файл валидным содержимым.
    #[tokio::test]
    async fn empty_and_truncated_files_start_fresh_and_heal_on_write() {
        let dir = tempfile::tempdir().unwrap();

        // Пустой файл (диск кончился в момент создания копии).
        let path = dir.path().join("empty.session");
        std::fs::write(&path, b"").unwrap();
        let s = FileSession::open(&path).unwrap();
        assert!(s.peer(PeerId::self_user()).await.unwrap().is_none());

        // Валидный файл, обрезанный на полпути (битый сектор / прерванное
        // копирование мимо атомарного rename).
        let full = dir.path().join("full.session");
        {
            let s = FileSession::open(&full).unwrap();
            s.set_dc_option(&dc(2, Some([9u8; 256]))).await.unwrap();
        }
        let bytes = std::fs::read(&full).unwrap();
        let cut = dir.path().join("cut.session");
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        let s = FileSession::open(&cut).unwrap();
        assert!(
            s.dc_option(2).unwrap().is_none_or(|d| d.auth_key.is_none()),
            "обрезанный файл не должен «частично» восстанавливать auth-key"
        );

        // Первая критичная запись перезаписывает битый файл валидным JSON.
        s.set_dc_option(&dc(4, Some([5u8; 256]))).await.unwrap();
        let healed = FileSession::open(&cut).unwrap();
        assert_eq!(healed.dc_option(4).unwrap().unwrap().auth_key, Some([5u8; 256]));
    }

    /// JSON валиден, но форма не наша (массив, строка, число, объект без
    /// обязательных полей) — это тоже «битый файл»: стартуем свежими, без
    /// паники и без ошибки.
    #[tokio::test]
    async fn valid_json_of_wrong_shape_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("arr", "[]"),
            ("str", "\"session\""),
            ("num", "42"),
            ("obj", "{\"version\":1}"),
        ] {
            let path = dir.path().join(format!("{name}.session"));
            std::fs::write(&path, body).unwrap();
            let s = FileSession::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(s.peer(PeerId::self_user()).await.unwrap().is_none(), "{name}");
        }
    }

    /// Путь оказался каталогом (в конфиге указали каталог вместо файла): это
    /// ошибка открытия, а не «свежая сессия» — иначе первая же запись
    /// попыталась бы сделать rename поверх каталога.
    #[test]
    fn open_on_a_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-as-dir");
        std::fs::create_dir(&path).unwrap();
        let err = FileSession::open(&path).unwrap_err();
        assert!(matches!(err, FileSessionError::Io(_)), "{err}");
    }

    /// Файл есть, но читать его нельзя (восстановлен из бэкапа с чужим
    /// владельцем/правами): это НЕ «свежая сессия» — auth-key потенциально
    /// жив, и молча затирать его нельзя, поэтому open обязан вернуть ошибку.
    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_an_error_not_a_fresh_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.session");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Под root'ом chmod не мешает чтению — сценарий не воспроизводится.
        if std::fs::read(&path).is_err() {
            let err = FileSession::open(&path).unwrap_err();
            assert!(matches!(err, FileSessionError::Io(_)), "{err}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// Каталог сессии на живом процессе «заняли» файлом (или он исчез):
    /// запись обязана вернуть ошибку, но состояние в памяти — уцелеть, мьютекс
    /// не отравиться, а после устранения помехи flush_now — дописать всё.
    #[tokio::test]
    async fn write_failure_keeps_state_in_memory_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("telegram.session");
        let s = FileSession::open(&path).unwrap(); // файла нет — свежая сессия
        // Место будущего каталога занимает обычный файл: create_dir_all падает.
        std::fs::write(dir.path().join("sub"), b"file").unwrap();

        let err = s.set_dc_option(&dc(2, Some([1u8; 256]))).await.unwrap_err();
        assert!(matches!(err, FileSessionError::Io(_)), "{err}");
        // Состояние в памяти не потеряно, последующие вызовы не паникуют.
        assert_eq!(s.dc_option(2).unwrap().unwrap().auth_key, Some([1u8; 256]));
        let target_dc = if s.home_dc_id().unwrap() == 5 { 4 } else { 5 };
        assert!(
            s.set_home_dc_id(target_dc).await.is_err(),
            "запись всё ещё невозможна"
        );
        assert_eq!(s.home_dc_id().unwrap(), target_dc, "home_dc в памяти обновился");

        // Помеха устранена — отложенное дожимается и переживает рестарт.
        std::fs::remove_file(dir.path().join("sub")).unwrap();
        s.flush_now().unwrap();
        let healed = FileSession::open(&path).unwrap();
        assert_eq!(healed.home_dc_id().unwrap(), target_dc);
        assert_eq!(healed.dc_option(2).unwrap().unwrap().auth_key, Some([1u8; 256]));
    }

    /// Параллельные критичные записи (auth-key приезжает наперегонки с
    /// миграцией DC): tmp+rename обязаны сериализоваться мьютексом — в конце
    /// валидный JSON со ВСЕМИ DC и без tmp-осколков.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_critical_writes_stay_coherent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let s = std::sync::Arc::new(FileSession::open(&path).unwrap());

        let mut handles = Vec::new();
        for i in 1..=8 {
            let s = std::sync::Arc::clone(&s);
            handles.push(tokio::spawn(async move {
                s.set_dc_option(&dc(i, Some([i as u8; 256]))).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        let reloaded = FileSession::open(&path).unwrap();
        for i in 1..=8 {
            assert_eq!(
                reloaded.dc_option(i).unwrap().unwrap().auth_key,
                Some([i as u8; 256]),
                "dc {i}"
            );
        }
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "остались tmp-файлы: {leftovers:?}");
    }

    /// Критичная запись обязана дожимать на диск и отложенные троттлингом
    /// дешёвые изменения: иначе падение сразу после неё потеряло бы pts/qts,
    /// хотя файл только что писали целиком.
    #[tokio::test]
    async fn critical_write_carries_pending_throttled_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let s = FileSession::open(&path).unwrap();

        s.set_update_state(UpdateState::Secondary { qts: 1 }).await.unwrap(); // пишет (окно открыто)
        s.set_update_state(UpdateState::Secondary { qts: 2 }).await.unwrap(); // отложено троттлингом
        s.set_dc_option(&dc(2, Some([8u8; 256]))).await.unwrap(); // критичная — пишет всё

        // Без flush_now: на диске обязаны быть и auth-key, и qts=2.
        let reloaded = FileSession::open(&path).unwrap();
        assert_eq!(reloaded.dc_option(2).unwrap().unwrap().auth_key, Some([8u8; 256]));
        assert_eq!(reloaded.updates_state().await.unwrap().qts, 2);
    }

    /// Повторный апдейт того же канала обязан заменять запись, а не плодить
    /// дубликаты: дубль с устаревшим pts ломал бы детекцию пропусков.
    #[tokio::test]
    async fn channel_update_state_replaces_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSession::open(&dir.path().join("telegram.session")).unwrap();
        s.set_update_state(UpdateState::Channel { id: 1, pts: 10 }).await.unwrap();
        s.set_update_state(UpdateState::Channel { id: 1, pts: 20 }).await.unwrap();
        s.set_update_state(UpdateState::Channel { id: 2, pts: 5 }).await.unwrap();

        let st = s.updates_state().await.unwrap();
        assert_eq!(st.channels.len(), 2, "{:?}", st.channels);
        let ch1 = st.channels.iter().find(|c| c.id == 1).unwrap();
        assert_eq!(ch1.pts, 20, "должен остаться свежий pts");
    }

    /// flush_now на чистой (немутированной) сессии — no-op: файл не создаётся.
    /// Иначе штатный shutdown до первого логина материализовал бы на диске
    /// пустую сессию.
    #[test]
    fn flush_now_on_clean_session_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let s = FileSession::open(&path).unwrap();
        s.flush_now().unwrap();
        assert!(!path.exists(), "чистый flush_now не должен создавать файл");
    }

    #[tokio::test]
    async fn throttled_writes_are_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.session");
        let s = FileSession::open(&path).unwrap();

        // Первая дешёвая мутация имеет право на запись (см. last_flush в open).
        s.set_update_state(UpdateState::Secondary { qts: 1 })
            .await
            .unwrap();
        assert!(path.exists());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Последующие в пределах окна не пишут файл повторно.
        for qts in 2..20 {
            s.set_update_state(UpdateState::Secondary { qts }).await.unwrap();
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            mtime,
            "троттлинг обязан склеивать частые мутации"
        );

        // flush_now отдаёт актуальное состояние на диск.
        s.flush_now().unwrap();
        let reloaded = FileSession::open(&path).unwrap();
        assert_eq!(reloaded.updates_state().await.unwrap().qts, 19);
    }
}
