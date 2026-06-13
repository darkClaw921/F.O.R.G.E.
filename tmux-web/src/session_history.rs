//! История сессий tmux — персистентное хранилище ранее виденных сессий.
//!
//! ### Назначение
//!
//! tmux хранит только *активные* сессии: после `tmux kill-session` (или
//! перезагрузки машины) информация о сессии исчезает. Этот модуль ведёт
//! отдельный JSON-журнал всех когда-либо виденных сессий, чтобы UI мог
//! показать «недавние»/«закрытые» сессии и быстро их восстановить.
//!
//! Для каждой сессии запоминается её имя, стартовый путь (`session_path`),
//! человекочитаемый ярлык папки (`folder_label` — basename корня проекта),
//! список окон (индекс + имя) и временные метки первого/последнего раза,
//! когда сессия была замечена живой.
//!
//! ### Хранилище
//!
//! Файл `<dir>/session_history.json`, где `dir` — data-каталог forge
//! (`~/.config/forge/`, тот же, откуда грузятся `themes.json` /
//! `notifier.json`; в `AppState` приходит как `themes_dir`). По соглашению с
//! остальными сторами каталог уже существует к моменту запуска devforge.
//!
//! [`HistoryStore`] — cheap-clone обёртка над `Arc<RwLock<Inner>>`: один
//! экземпляр кладётся в `AppState`, клонируется в воркеры/хендлеры без
//! копирования данных. Запись на диск — атомарная (temp-файл + `rename`),
//! идентично [`crate::themes`] и [`crate::notifier_config`]: при краше во
//! время записи на диске остаётся целый старый файл, а не половина нового.
//!
//! ### Политика отказоустойчивости
//!
//! [`HistoryStore::load`] никогда не паникует: отсутствующий файл или битый
//! JSON приводят к пустому стору (с `tracing::warn!` для битого файла).
//! Это тот же инвариант «битый файл не блокирует старт», что и у остальных
//! конфигов проекта.
//!
//! ### Жизненный цикл (Phase 2+)
//!
//! [`capture_now`] — единая точка снятия снимка: опрашивает tmux
//! ([`crate::tmux::list_sessions`] + [`crate::tmux::list_windows`]) и делает
//! upsert в стор. Переиспользуется периодическим воркером и shutdown-хуком.
//! В Phase 1 модуль не интегрирован в `AppState`/роуты — только зарегистрирован
//! как `mod session_history;`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::tmux::{self, SessionInfo, WindowInfo};

/// Имя файла-хранилища внутри data-каталога forge.
const FILE_NAME: &str = "session_history.json";

/// Максимум записей в журнале истории. Сессии приходят/уходят, ключ —
/// `name + path`, поэтому без ограничения карта растёт неограниченно (каждая
/// уникальная сессия за всё время существования forge). Держим только самые
/// свежие по `last_seen`, лишнее (давно не виденное) выбрасываем.
const MAX_HISTORY_ENTRIES: usize = 500;

/// Описание одного окна сессии в истории.
///
/// Хранит только стабильные, человеко-значимые поля (`index` + `name`).
/// `active`/`panes` из [`WindowInfo`] сознательно опускаются — они меняются
/// от снимка к снимку и не нужны для отображения истории.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryWindow {
    /// Индекс окна в сессии (`#{window_index}`).
    pub index: u32,
    /// Имя окна (`#{window_name}`).
    pub name: String,
}

/// Одна запись истории — сессия и её окна на момент последнего снимка.
///
/// Ключ записи в хранилище — `name + "\0" + path` (см.
/// [`HistorySession::key`]): он защищает от коллизий одинаковых имён сессий,
/// запущенных в разных каталогах.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistorySession {
    /// Имя tmux-сессии (`#{session_name}`).
    pub name: String,
    /// Стартовый cwd сессии (`#{session_path}`).
    pub path: String,
    /// Человекочитаемый ярлык папки — basename корня проекта
    /// (`paths::resolve_root(path)`). `None`, если basename вычислить не
    /// удалось (например, путь — корень файловой системы).
    pub folder_label: Option<String>,
    /// Окна сессии на момент последнего снимка (перезаписываются целиком).
    pub windows: Vec<HistoryWindow>,
    /// Unix-время (секунды) первого раза, когда сессия была замечена.
    pub first_seen: i64,
    /// Unix-время (секунды) последнего раза, когда сессия была замечена.
    pub last_seen: i64,
}

impl HistorySession {
    /// Ключ записи: `name + "\0" + path`. `\0` не может встретиться ни в имени
    /// сессии, ни в пути, поэтому конкатенация однозначна.
    fn key(name: &str, path: &str) -> String {
        format!("{name}\0{path}")
    }
}

/// Внутреннее изменяемое состояние стора под `RwLock`.
#[derive(Debug)]
struct Inner {
    /// Записи по ключу `name\0path`.
    sessions: HashMap<String, HistorySession>,
    /// Путь к `session_history.json`.
    file: PathBuf,
}

/// Персистентное хранилище истории сессий.
///
/// Cheap-clone: внутри `Arc<RwLock<Inner>>`, `clone()` лишь увеличивает
/// счётчик ссылок. Один экземпляр на процесс (живёт в `AppState`).
#[derive(Debug, Clone)]
pub struct HistoryStore {
    inner: Arc<RwLock<Inner>>,
}

impl HistoryStore {
    /// Загружает историю из `<dir>/session_history.json`.
    ///
    /// Отсутствие файла → пустой стор (это нормальный первый запуск).
    /// Ошибка чтения/парсинга → пустой стор + `tracing::warn!` (битый файл
    /// не должен блокировать работу devforge). Дефолт на диск не пишется —
    /// файл появится при первом успешном [`HistoryStore::persist`].
    pub fn load(dir: &Path) -> HistoryStore {
        let file = dir.join(FILE_NAME);
        let sessions = match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice::<Vec<HistorySession>>(&bytes) {
                Ok(list) => list
                    .into_iter()
                    .map(|s| (HistorySession::key(&s.name, &s.path), s))
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        path = %file.display(),
                        error = ?e,
                        "session_history.json parse failed; starting with empty history"
                    );
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                tracing::warn!(
                    path = %file.display(),
                    error = ?e,
                    "session_history.json read failed; starting with empty history"
                );
                HashMap::new()
            }
        };

        HistoryStore {
            inner: Arc::new(RwLock::new(Inner { sessions, file })),
        }
    }

    /// Upsert снимка текущих сессий с их окнами.
    ///
    /// Для каждой пары `(SessionInfo, windows)`:
    /// - если запись с таким ключом уже есть — обновляются `last_seen`
    ///   (текущее время), `windows` и `folder_label`; `first_seen`
    ///   сохраняется;
    /// - если записи нет — создаётся новая с `first_seen = last_seen = now`.
    ///
    /// После мутации — атомарная запись на диск через [`HistoryStore::persist`].
    pub fn snapshot(&self, sessions: &[(SessionInfo, Vec<WindowInfo>)]) {
        let now = now_unix_secs();
        {
            let mut inner = self.inner.write().expect("HistoryStore lock poisoned");
            for (info, wins) in sessions {
                let key = HistorySession::key(&info.name, &info.path);
                let folder_label = folder_label_for(&info.path);
                let windows: Vec<HistoryWindow> = wins
                    .iter()
                    .map(|w| HistoryWindow {
                        index: w.index,
                        name: w.name.clone(),
                    })
                    .collect();

                inner
                    .sessions
                    .entry(key)
                    .and_modify(|existing| {
                        existing.last_seen = now;
                        existing.windows = windows.clone();
                        existing.folder_label = folder_label.clone();
                    })
                    .or_insert_with(|| HistorySession {
                        name: info.name.clone(),
                        path: info.path.clone(),
                        folder_label,
                        windows,
                        first_seen: now,
                        last_seen: now,
                    });
            }

            // Ограничиваем журнал: оставляем MAX_HISTORY_ENTRIES самых свежих
            // по last_seen, выкидываем самые старые. Дёшево, т.к. порог
            // превышается редко (только после очень долгой работы).
            if inner.sessions.len() > MAX_HISTORY_ENTRIES {
                let mut by_recency: Vec<(String, i64)> = inner
                    .sessions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.last_seen))
                    .collect();
                // Сортируем по last_seen убыв.; всё после порога удаляем.
                by_recency.sort_by(|a, b| b.1.cmp(&a.1));
                for (key, _) in by_recency.into_iter().skip(MAX_HISTORY_ENTRIES) {
                    inner.sessions.remove(&key);
                }
            }
        }
        self.persist();
    }

    /// Возвращает все записи истории, отсортированные по `last_seen` убыв.
    /// (самые свежие — первыми).
    pub fn list(&self) -> Vec<HistorySession> {
        let inner = self.inner.read().expect("HistoryStore lock poisoned");
        let mut out: Vec<HistorySession> = inner.sessions.values().cloned().collect();
        out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        out
    }

    /// Удаляет запись истории по ключу `name + path` и сохраняет результат.
    ///
    /// No-op (но всё равно с persist), если записи нет.
    pub fn remove(&self, name: &str, path: &str) {
        {
            let mut inner = self.inner.write().expect("HistoryStore lock poisoned");
            inner.sessions.remove(&HistorySession::key(name, path));
        }
        self.persist();
    }

    /// Атомарно сохраняет текущее состояние в `session_history.json`.
    ///
    /// Записи сериализуются как JSON-массив (отсортированный по `last_seen`
    /// убыв., как и [`HistoryStore::list`]). Стратегия идентична остальным
    /// сторам: пишем во временный `<file>.tmp`, затем `rename` поверх (на
    /// POSIX атомарно в пределах mount-point). Каталог создаётся при
    /// необходимости. Ошибки логируются через `tracing::warn!`, но не
    /// паникуют — потеря снимка не должна валить процесс.
    pub fn persist(&self) {
        // Сериализуем снимок и копируем целевой путь ПОД read-guard'ом, затем
        // СРАЗУ дропаем guard — блокирующий fs::write/rename выполняется уже
        // без удержания RwLock, чтобы не сериализовать конкурентные snapshot/
        // list/remove (watcher_loop вызывает snapshot каждый тик ~1.5с).
        let (body, file) = {
            let inner = self.inner.read().expect("HistoryStore lock poisoned");

            let mut list: Vec<&HistorySession> = inner.sessions.values().collect();
            list.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));

            let body = match serde_json::to_vec_pretty(&list) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = ?e, "failed to serialize session history");
                    return;
                }
            };
            (body, inner.file.clone())
        };

        if let Some(parent) = file.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        path = %parent.display(),
                        error = ?e,
                        "failed to create session_history parent dir"
                    );
                    return;
                }
            }
        }

        let mut tmp = file.clone();
        let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);

        if let Err(e) = std::fs::write(&tmp, &body) {
            tracing::warn!(path = %tmp.display(), error = ?e, "failed to write session_history tmp");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &file) {
            tracing::warn!(
                from = %tmp.display(),
                to = %file.display(),
                error = ?e,
                "failed to rename session_history tmp into place"
            );
        }
    }
}

/// Снимает текущее состояние tmux и сохраняет его в `store`.
///
/// Опрашивает [`crate::tmux::list_sessions`], затем для каждой сессии —
/// [`crate::tmux::list_windows`], собирая `Vec<(SessionInfo, Vec<WindowInfo>)>`,
/// и вызывает [`HistoryStore::snapshot`]. Сессии, для которых `list_windows`
/// вернул ошибку (например, сессия исчезла между двумя вызовами), включаются
/// в снимок с пустым списком окон.
///
/// Если tmux-сервер не запущен, `list_sessions` вернёт пустой список — снимок
/// будет пустым, но `snapshot` всё равно выполнит persist (no-op изменений).
///
/// Переиспользуется периодическим воркером и shutdown-хуком (Phase 2+).
pub async fn capture_now(store: &HistoryStore) {
    let sessions = match tmux::list_sessions().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "capture_now: failed to list tmux sessions");
            return;
        }
    };

    let mut collected: Vec<(SessionInfo, Vec<WindowInfo>)> = Vec::with_capacity(sessions.len());
    for info in sessions {
        let windows = match tmux::list_windows(&info.name).await {
            Ok(w) => w,
            Err(e) => {
                tracing::debug!(
                    session = %info.name,
                    error = ?e,
                    "capture_now: failed to list windows; recording session with no windows"
                );
                Vec::new()
            }
        };
        collected.push((info, windows));
    }

    store.snapshot(&collected);
}

/// Текущее Unix-время в секундах. При сбое системных часов (до эпохи) — `0`.
fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Вычисляет `folder_label` — basename корня проекта для стартового пути
/// сессии (`paths::resolve_root(path)`). Возвращает `None`, если basename
/// извлечь не удалось (пустой путь или корень ФС).
fn folder_label_for(path: &str) -> Option<String> {
    let root = paths::resolve_root(Path::new(path));
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-history-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn session(name: &str, path: &str) -> SessionInfo {
        SessionInfo {
            name: name.to_string(),
            id: "$0".to_string(),
            attached: 0,
            windows: 1,
            created: 0,
            path: path.to_string(),
            session_group: None,
        }
    }

    fn window(index: u32, name: &str) -> WindowInfo {
        WindowInfo {
            index,
            name: name.to_string(),
            active: true,
            panes: 1,
        }
    }

    #[test]
    fn load_missing_returns_empty() {
        let dir = tempdir("missing");
        let store = HistoryStore::load(&dir);
        assert!(store.list().is_empty());
        // load не создаёт файл.
        assert!(!dir.join(FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_returns_empty() {
        let dir = tempdir("corrupt");
        std::fs::write(dir.join(FILE_NAME), b"{not valid json").unwrap();
        let store = HistoryStore::load(&dir);
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_caps_history_to_max_entries() {
        let dir = tempdir("cap");
        let store = HistoryStore::load(&dir);
        // Заливаем заметно больше порога уникальных сессий.
        let total = MAX_HISTORY_ENTRIES + 50;
        for i in 0..total {
            let s = session(&format!("sess-{i}"), &format!("/p/{i}"));
            store.snapshot(&[(s, vec![window(0, "main")])]);
        }
        let list = store.list();
        assert_eq!(
            list.len(),
            MAX_HISTORY_ENTRIES,
            "history must be capped at MAX_HISTORY_ENTRIES"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_upserts_without_duplicates() {
        let dir = tempdir("upsert");
        let store = HistoryStore::load(&dir);

        let s = session("alpha", "/tmp/alpha");
        store.snapshot(&[(s.clone(), vec![window(0, "main")])]);
        let first = store.list();
        assert_eq!(first.len(), 1);
        let first_seen = first[0].first_seen;
        assert_eq!(first[0].windows.len(), 1);

        // Повторный снимок той же сессии не плодит дубль, обновляет окна.
        store.snapshot(&[(s.clone(), vec![window(0, "main"), window(1, "logs")])]);
        let second = store.list();
        assert_eq!(second.len(), 1, "upsert must not create a duplicate");
        assert_eq!(second[0].windows.len(), 2);
        assert_eq!(second[0].first_seen, first_seen, "first_seen must be preserved");
        assert!(second[0].last_seen >= first_seen);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_name_different_path_are_distinct() {
        let dir = tempdir("collision");
        let store = HistoryStore::load(&dir);
        store.snapshot(&[
            (session("dev", "/tmp/a"), vec![]),
            (session("dev", "/tmp/b"), vec![]),
        ]);
        assert_eq!(store.list().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sorted_by_last_seen_desc() {
        let dir = tempdir("sort");
        let store = HistoryStore::load(&dir);

        // Внедряем записи с разными last_seen напрямую.
        {
            let mut inner = store.inner.write().unwrap();
            for (name, last) in [("old", 100i64), ("new", 300), ("mid", 200)] {
                let key = HistorySession::key(name, "/p");
                inner.sessions.insert(
                    key,
                    HistorySession {
                        name: name.to_string(),
                        path: "/p".to_string(),
                        folder_label: None,
                        windows: vec![],
                        first_seen: last,
                        last_seen: last,
                    },
                );
            }
        }
        let names: Vec<String> = store.list().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["new", "mid", "old"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_and_reload_roundtrip() {
        let dir = tempdir("roundtrip");
        let store = HistoryStore::load(&dir);
        store.snapshot(&[(session("gamma", "/tmp/gamma"), vec![window(0, "edit")])]);
        assert!(dir.join(FILE_NAME).exists(), "persist must write valid file");

        let reloaded = HistoryStore::load(&dir);
        let list = reloaded.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "gamma");
        assert_eq!(list[0].path, "/tmp/gamma");
        assert_eq!(list[0].windows, vec![HistoryWindow { index: 0, name: "edit".into() }]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_drops_entry() {
        let dir = tempdir("remove");
        let store = HistoryStore::load(&dir);
        store.snapshot(&[(session("toremove", "/tmp/x"), vec![])]);
        assert_eq!(store.list().len(), 1);
        store.remove("toremove", "/tmp/x");
        assert!(store.list().is_empty());
        // Удаление persist'нуто.
        let reloaded = HistoryStore::load(&dir);
        assert!(reloaded.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
