Phase 3 code-review fixes (LOW severity, эпик forge-4wyq). 17 задач.

ClaudeRunner.running (claude/mod.rs): AbortHandle вставляется под удержанием Mutex после spawn; RunGuard(Drop) снимает регистрацию на всех путях выхода таски (return/timeout/abort/panic). Гонка remove-до-insert исключена.

parse_line stderr (claude/mod.rs): кольцевой буфер последних 20 строк stderr; при пустом stdout или таймауте Error содержит stderr-хвост.

find_due LIMIT (db/repo/autonomous.rs): FIND_DUE_LIMIT=64, пачковая обработка просроченных задач.

session_history cap (session_history.rs): MAX_HISTORY_ENTRIES=500, в snapshot() обрезаем по last_seen.

daemon PID-recycling (daemon.rs): pid-файл хранит '<pid> <start_time>'; process_start_time (macOS proc_pidinfo PROC_PIDTBSDINFO / Linux /proc/pid/stat field22); is_recorded_process_alive сверяет перед kill. Обратная совместимость со старым форматом.

qr_print token masking (qr_print.rs): при не-TTY маскирует токен (mask_token_in_url) и не печатает QR. daemon.rs создаёт лог с 0600.

summarize_day (memory/mod.rs): global-ветка исключает __autonomous__/ чаты через substr(session_id,1,15).

register_worker (state.rs): async, берёт workers-lock напрямую; spawn_workers→async. Устранена гонка с shutdown_workers.

git --since (git.rs): format!('@{v}') явный unix-timestamp, унифицировано с echo_host.rs.

notifier reconcile (notifier.rs): на старте reconcile_wait_previous_on_startup сверяет last_promoted_open_id через 'br show'; закрытые offline задачи продвигают очередь.

remote_proxy Connection (remote_proxy.rs): collect_connection_tokens парсит Connection-заголовок (RFC 7230), удаляет перечисленные хедеры.

restore_one_session (main.rs + tmux.rs): new_window_at/move_window воссоздают окна по явным оригинальным индексам (разрежённые раскладки 0,2,5).

Frontend: epoch-счётчики в fetchGitCommits/fetchTasks/fetchTodos (stale-response гонки); fetchTasks/Todos сохраняют снапшот при HTTP-ошибке; switchSession fallback сохраняет origin; escapeText для issue.id и textContent для e.message; xterm завендорен в static/vendor/xterm/ вместо CDN.