# main::AppState

Глобальное состояние axum-приложения для tmux-web (src/main.rs).

Поля:
- projects: Arc<RwLock<ProjectStore>> — реестр проектов (multi-project, Phase 6.B). Read-locks для GET-эндпоинтов идут параллельно, write-locks (POST/DELETE) сериализуются. Persistence через ProjectStore::save() под write-lock'ом.
- tasks_tx: broadcast::Sender<TaskEvent> — broadcast(64) для realtime-стрима событий beads-watcher (Phase 6.D). Подписчики /ws/tasks делают subscribe() на каждое соединение.
- active_path_tx: Arc<watch::Sender<PathBuf>> — watch-channel с last-value семантикой; отправляет новый active.path в фоновый tasks_watcher при смене активного проекта.
- attention: Arc<attention::AttentionState> (Phase 7) — разделяемое состояние «у каких сессий открыт Claude permission prompt». Background attention::watcher_loop пишет сюда каждые 1.5с; HTTP-handler get_sessions читает snapshot и возвращает needs_attention в SessionDto.

Конструктор/инициализация:
В main() сразу после загрузки ProjectStore и создания tasks_tx/active_path_tx собирается AppState{...} с attention: Arc::new(attention::AttentionState::new()). Затем спавнятся два фоновых tokio-task'а: tasks_watcher::run_watcher(active_path_rx, tasks_tx) и attention::watcher_loop(app_state.projects.clone(), app_state.attention.clone()).

#[derive(Clone)] — клон дешёвый (только Arc.clone()), используется axum через .with_state(app_state).
