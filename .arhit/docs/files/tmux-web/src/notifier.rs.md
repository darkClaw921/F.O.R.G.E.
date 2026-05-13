# tmux-web/src/notifier.rs

Phase 2 — фоновая подсистема нотификаций при promote TODO → bd-task. Файл tmux-web/src/notifier.rs (~570 строк).

## Назначение
Когда пользователь promote'ит TODO-карточку в реальную bd-задачу, нужно отправить текстовое уведомление в активную tmux-сессию проекта с заданной задержкой и в одном из трёх режимов:
- Immediate — сразу tmux::send_keys.
- Delayed{fire_at_unix_ms} — tokio::time::sleep_until(fire_at) затем send.
- WaitPrevious{previous_task_id} — ждём пока bd-задача с указанным id перейдёт в status=closed (через broadcast от tasks_watcher). Это очередь FIFO per project: пока предыдущий промоут не закрыт — следующий висит.

## Ключевые элементы
- enum NotifyMode { Immediate, Delayed{fire_at_unix_ms: u64}, WaitPrevious{previous_task_id: Option<String>} } — сериализуется в JSON с tag='kind'.
- struct NotifyJob { id: String (UUID v4), project_id, task_id, target_session, text, mode, created_at_unix_ms } — единица очереди.
- enum NotifyCommand { Enqueue(NotifyJob) } — команды в mpsc.
- struct NotifyHandle { tx: mpsc::Sender<NotifyCommand> } — cheap-clonable handle, хранится в AppState. Метод enqueue(job).await.
- pub fn start(project_root: PathBuf, task_events_rx: broadcast::Receiver<TaskEvent>) -> NotifyHandle — спавнит notifier_loop, возвращает handle.
- pub fn new_job(project_id, task_id, target_session, text, mode) -> NotifyJob — конструктор с UUID и timestamp.

## notifier_loop (фоновый task)
select! по трём веткам:
1. cmd_rx.recv() — новые команды Enqueue.
2. sleep_until(next_delayed_deadline) — таймер ближайшего Delayed-job'а.
3. task_events_rx.recv() — TaskEvent::Upsert от tasks_watcher; фильтр по status=='closed' триггерит handle_task_closed для wait_previous.

## Persist
Файл <project_root>/.forge/notify_state.json:
- pending: Vec<NotifyJob> — все ждущие jobs (Delayed с будущим fire_at, WaitPrevious в очереди).
- wait_queues: HashMap<project_id, VecDeque<job_id>> — FIFO очередь per project.
- last_promoted_open_id: HashMap<project_id, task_id> — последний промоутнутый и ещё не закрытый task.

Atomic save через tempfile + rename (паттерн из todos.rs/projects.rs). State сохраняется ДО fire (защита от kill -9).

## Запуск и восстановление
При start: load_state из диска, fire_due_immediate_and_overdue (просроченные Delayed + Immediate из pending fire'ятся сразу).

## Доставка
fire_job вызывает tmux::send_keys с retry x3 (backoff 500/1000/2000ms). Полный фейл — лог error!, job дропается, loop не падает.

## WaitPrevious-логика
- handle_enqueue: если previous_task_id уже не tracked в last_promoted_open_id и очередь пуста — fire сразу. Иначе кладём в wait_queues и pending.
- handle_task_closed: при Upsert с status=closed для task_id, который равен last_promoted_open_id[pid] — снять tracking, взять head из wait_queues, fire его, обновить last_promoted_open_id.

## Тесты
6 unit tests: save_load_empty_roundtrip, save_load_with_jobs, next_delayed_deadline_picks_earliest, instant_for_past_returns_now, notify_mode_serialization_roundtrip, new_job_generates_uuid. Все проходят.

## Зависимости
- tokio::sync::{broadcast, mpsc}, tokio::time::{sleep_until, Instant}.
- crate::tasks::TaskEvent (для подписки).
- crate::tmux::send_keys (для доставки).
- uuid v4 (Cargo.toml).
- serde/serde_json (persist).

## Используется
- main.rs: notifier::start() в bootstrap, NotifyHandle в AppState.notify.
- Phase 3 (TBD): POST /api/todos/:id/promote вызывает state.notify.enqueue(notifier::new_job(...)) после br create.
