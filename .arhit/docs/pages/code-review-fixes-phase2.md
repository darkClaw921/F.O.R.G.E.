Фаза 2 код-ревью фиксов (эпик forge-4wyq, 19 задач MEDIUM, 2026-06-11).

BACKEND (tmux-web):
- ws_tasks.rs: при None из notify_rx заменяем receiver на вечно-pending (mpsc+forget) — устранён busy-loop 100% CPU (forge-skde). Watcher стартует ДО snapshot (forge-3p1q).
- ws_todos.rs: subscribe ДО snapshot — события в окне не теряются (forge-3p1q).
- attention.rs: добавлен map.retain(live) в cleanup (forge-gt1k); info-логи (pane hash changed, indicator summary) понижены до debug (forge-06oe).
- daemon.rs read_tail: seek от конца + растущий буфер вместо read_to_string (forge-06oe).
- tmux.rs send_keys: добавлен '--' перед литералом (forge-mhx9).
- main.rs urlencode_minimal: blocklist→allowlist (alnum/-/_/./~), '/'→%2F закрывает path-инъекцию (forge-vt68).
- todos.rs/session_history.rs/notifier_config.rs/remotes.rs: fs::write вынесен за пределы lock (serialize под guard, write после drop) (forge-lce4).

ECHO PLUGIN:
- db/repo/messages.rs list_by_session: ORDER BY created_at DESC,rowid DESC LIMIT + reverse → последние N сообщений (forge-ubyy).
- scheduler+routes/autonomous.rs: RunningSet перенесён в EchoState; run_now проверяет анти-дубль; run_task в catch_unwind (forge-rkuo, forge-1y9z).
- claude/events.rs: ClaudeEvent::Result.is_error (is_error/subtype=error_*); RunResult.is_error; runner финиширует error (forge-vh7y).
- ws/mod.rs: Error/Result{is_error}→ServerMsg::Error клиенту; пустой assistant не пишется (forge-0ob8). Lagged→ServerMsg::Resync (forge-ji1b).
- routes/next_step.rs send: требует активного предложения, отклоняет произвольный text (forge-4ww0).

FRONTEND (static/js):
- echo/main.js openChat: проверка activeConversationId после await (forge-cfm3); handler resync перечитывает переписку (forge-ji1b).
- daily-summary.js: токен дня после await (forge-ey19).
- core/markdown.js: isSafeUrl allowlist http/https/mailto, иначе текст (forge-t3ib).
- settings/notifications-tab.js+modal.js: fetchNotifierConfig возвращает {ok,error}; при ошибке UI блокирует форму, Retry (forge-gwsr).