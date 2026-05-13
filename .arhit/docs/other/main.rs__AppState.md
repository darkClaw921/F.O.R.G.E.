# main.rs::AppState

Глобальное состояние axum-приложения. Поля: projects (Arc<RwLock<ProjectStore>>), tasks_tx (broadcast::Sender<TaskEvent>), active_path_tx (Arc<watch::Sender<PathBuf>>), attention (Arc<AttentionState>), notify (NotifyHandle, Phase 2), todos (TodoStore, Phase 3), todos_tx (broadcast::Sender<TodoEvent>, Phase 3 — sender'ы REST handlers, subscribers WS /ws/todos).
