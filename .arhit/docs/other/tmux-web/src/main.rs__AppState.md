# tmux-web/src/main.rs::AppState

Глобальное состояние axum-приложения. Cheap-clonable (все поля Arc/Sender внутри).

Поля:
- projects: Arc<RwLock<ProjectStore>> — реестр проектов с активным проектом. Чтения параллельны, write сериализованы.
- tasks_tx: broadcast::Sender<TaskEvent> — Phase 6.D: tasks_watcher пушит сюда события при изменениях .beads/issues.jsonl. WS /ws/tasks подписывается на subscribe() в каждом соединении.
- active_path_tx: Arc<watch::Sender<PathBuf>> — watch-sender для пересоздания tasks_watcher при смене активного проекта. Любой эндпоинт, меняющий active project, должен отправить новый path.
- attention: Arc<attention::AttentionState> — shared state needs-attention флагов. Background attention::watcher_loop пишет каждые 1.5с; HTTP-handler get_sessions читает snapshot.
- notify: notifier::NotifyHandle — Phase 2: handle к фоновому notifier_loop. Используется Phase 3 эндпоинтом POST /api/todos/:id/promote, который ставит NotifyJob в очередь через notify.enqueue(job).await.

Передаётся в handlers через axum::extract::State<AppState>.
