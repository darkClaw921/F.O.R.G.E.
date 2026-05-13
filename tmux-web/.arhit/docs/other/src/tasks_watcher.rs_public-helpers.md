# src/tasks_watcher.rs#public-helpers

В Phase 6.D добавлены pub re-export'ы из tasks_watcher.rs для использования в ws_tasks.rs (per-connection watcher):

- pub fn find_beads_dir(start: &Path) -> Option<PathBuf> — walk up до .beads/ (как делает br CLI). Возвращает первый найденный или None если дошли до корня FS.
- pub fn relevant_event(ev: &notify::Event) -> bool — фильтр FS-событий: интересны только paths оканчивающиеся на issues.jsonl, issues.jsonl.* (tmp atomic-rename) или *.jsonl. Игнорирует beads.db и beads.db-wal.
- pub const DEBOUNCE_MS: u64 = 200 — tail-debounce окно для группировки burst'ов notify-событий (br sync обычно ~50ms, 200ms даёт запас).

Глобальный run_watcher() остаётся (используется notifier.rs для process-wide задачи трекинга). Per-connection watcher в ws_tasks.rs — independent, дублирует логику инстанцирования но с тем же debounce/filter/find.
