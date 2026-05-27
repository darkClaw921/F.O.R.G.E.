# session_history

Модуль tmux-web/src/session_history.rs — персистентное хранилище истории tmux-сессий. tmux держит только активные сессии (после kill-session/перезагрузки данные теряются), поэтому модуль ведёт отдельный JSON-журнал всех когда-либо виденных сессий, чтобы UI мог показать недавние/закрытые сессии и быстро восстановить их.

СТРУКТУРЫ:
- HistoryWindow { index: u32, name: String } — окно сессии в истории (только стабильные поля, без active/panes из tmux::WindowInfo).
- HistorySession { name, path, folder_label: Option<String>, windows: Vec<HistoryWindow>, first_seen: i64, last_seen: i64 } — одна запись истории. Ключ записи = name + \0 + path (HistorySession::key), защищает от коллизий одинаковых имён в разных каталогах. folder_label — basename корня проекта (paths::resolve_root(path)).
- HistoryStore — cheap-clone обёртка над Arc<RwLock<Inner>>, Inner { sessions: HashMap<String, HistorySession>, file: PathBuf }. Один экземпляр на процесс, кладётся в AppState (Phase 2).

МЕТОДЫ HistoryStore:
- load(dir: &Path) -> HistoryStore — читает <dir>/session_history.json (FILE_NAME). Отсутствие файла → пустой стор; битый JSON или ошибка чтения → пустой стор + tracing::warn. Файл на диск не пишется до первого persist (инвариант 'битый файл не блокирует старт', как у themes/notifier_config).
- snapshot(&self, sessions: &[(SessionInfo, Vec<WindowInfo>)]) — upsert по ключу: существующим обновляет last_seen=now, windows, folder_label (сохраняя first_seen); новым ставит first_seen=last_seen=now. Затем persist().
- list(&self) -> Vec<HistorySession> — все записи, отсортированы по last_seen убыв.
- remove(&self, name, path) — удаляет запись по ключу + persist().
- persist(&self) — атомарная запись JSON-массива (отсортирован по last_seen убыв.) через temp-файл <file>.tmp + std::fs::rename, идентично themes.rs/notifier_config.rs. Ошибки логируются warn, не паникуют.

ФУНКЦИИ:
- capture_now(store: &HistoryStore) async — единая точка снятия снимка: tmux::list_sessions() + для каждой tmux::list_windows() → собирает Vec<(SessionInfo, Vec<WindowInfo>)> → store.snapshot(). Сессии с ошибкой list_windows записываются с пустым списком окон. Переиспользуется воркером и shutdown-хуком (Phase 2+).
- now_unix_secs() -> i64 — текущее Unix-время в секундах через std::time::SystemTime (проект использует SystemTime, НЕ chrono — chrono нет в зависимостях tmux-web).
- folder_label_for(path) -> Option<String> — basename(paths::resolve_root(path)).

ЗАВИСИМОСТИ: crate::tmux (SessionInfo, WindowInfo, list_sessions, list_windows), crate::paths (resolve_root), serde, std::sync::{Arc, RwLock}, std::time::SystemTime.

ХРАНИЛИЩЕ: ~/.config/forge/session_history.json (тот же data-каталог, что themes.json/notifier.json; в AppState приходит как themes_dir).

PHASE 1: модуль зарегистрирован в main.rs как 'mod session_history;' с #[allow(dead_code)] (API интегрируется в AppState/роуты в Phase 2). Не интегрирован в AppState/endpoints. 7 unit-тестов покрывают load/snapshot/list/remove/persist/коллизии ключей.
