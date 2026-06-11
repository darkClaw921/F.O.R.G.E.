# tmux-web/src/tasks_watcher.rs

Мульти-корневой фоновый file-watcher для .beads/issues.jsonl. Переписан с одно-корневой схемы (active_path) на мульти-корневую — фикс бага «авто-запуск следующей todo не сработал, хотя задача закрыта»: старый watcher следил только за каталогом запуска сервера (active_path_tx после старта никогда не обновлялся), и closed-события задач других проектов не доходили до auto_promote::run и notifier (wait_previous) — цепочка авто-промоута не двигалась нигде, кроме стартового проекта.

Публичный API:
- run_multi_watcher(roots_rx: watch::Receiver<BTreeSet<PathBuf>>, tasks_tx: broadcast::Sender<TaskEvent>) — главный цикл. Получает набор путей-кандидатов из roots-канала (наполняет collector-task в main.rs: cwd всех tmux-сессий + initial cwd процесса + корни активных auto_chain, пересборка каждые 5с, send_if_modified). Каждый кандидат резолвится через find_beads_dir; дедуп по фактическому .beads/-пути (сессии одного репо/подкаталогов -> один watcher). На изменение набора: spawn watch_root для новых корней, JoinHandle::abort для исчезнувших. Завершается при закрытии roots-канала.
- watch_root(snapshot_root, beads_dir, tasks_tx) — watcher одного корня: initial snapshot (baseline, не бродкастится) -> notify::recommended_watcher на .beads/ (NonRecursive, mpsc::unbounded как EventHandler) -> 200ms tail-debounce -> tasks::snapshot + diff_issues -> broadcast TaskEvent. Живёт до abort.
- find_beads_dir(start) — поиск .beads/ вверх по родителям (зеркалит br).
- relevant_event(ev) — фильтр fs-событий (issues.jsonl и *.jsonl/tmp).
- DEBOUNCE_MS = 200.

Подписчики глобального tasks_tx: notifier.rs (wait_previous) и auto_promote::run. UI (/ws/tasks) канал НЕ использует — у него per-connection watcher'ы на ?path= клиента (ws_tasks.rs), поэтому мульти-корневой broadcast не утекает в чужие канбаны.

Старый API run_watcher(active_path_rx, tasks_tx) удалён.
