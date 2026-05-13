# tmux-web/src/notifier.rs::start

pub fn start(project_root: PathBuf, task_events_rx: broadcast::Receiver<TaskEvent>) -> NotifyHandle — entry point Phase 2 notifier подсистемы.

Что делает:
1. Создаёт mpsc::channel<NotifyCommand>(256).
2. Спавнит фоновый notifier_loop через tokio::spawn.
3. Возвращает NotifyHandle (обёртка над Sender'ом).

Вызывается один раз в main() при старте сервера. project_root используется для размещения <root>/.forge/notify_state.json. task_events_rx получается через AppState.tasks_tx.subscribe() и должен быть subscribed ДО передачи tasks_tx в tasks_watcher (иначе ранние closed-events могут быть пропущены, broadcast хранит только 64).

Lifetime: Loop живёт пока жив tx в NotifyHandle (т.е. пока AppState не дропнут). При drop — последний save уже сделан после каждой мутации.
