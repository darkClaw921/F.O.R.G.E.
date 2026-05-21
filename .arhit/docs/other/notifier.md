# notifier

Фоновый notifier_loop (tmux-web/src/notifier.rs) — доставка notify-сообщений при promote TODO → bd-task.

## Phase 3 изменения
- NotifyJob.project_id → root_path (rename, явная семантика cwd-only).
- #[serde(alias = "project_id")] обеспечивает обратную совместимость со старыми .forge/notify_state.json — pending-jobs не теряются при апгрейде.
- Notifier больше не делает lookup в ProjectStore: target_session приходит в job напрямую от вызывающего (promote_todo + NotifierConfig.session).
- Ключи map'ов wait_queues / last_promoted_open_id — теперь root_path.

## NotifyMode
- Immediate — мгновенный tmux::send_keys (с retry x3).
- Delayed { fire_at_unix_ms } — sleep_until + send.
- WaitPrevious { previous_task_id } — FIFO-очередь per root_path: ждать пока предыдущий promoted-issue не закроется (TaskEvent::Upsert со status=closed).

## API
- start(project_root, task_events_rx) -> NotifyHandle — spawn loop.
- NotifyHandle.enqueue(job) — отправить Enqueue-команду через mpsc.
- new_job(root_path, task_id, target_session, text, mode) — конструктор с UUID/timestamp.

## Persistence
<project_root>/.forge/notify_state.json — pending jobs + wait_queues + last_promoted_open_id. Atomic save (tempfile + rename). Битый файл ⇒ старт с defaults без падения. На старте: проигрываются Immediate и просроченные Delayed.

## Retry policy
fire_job: backoff 500/1000/2000ms, 3 попытки. После — error в лог, job дропается.
