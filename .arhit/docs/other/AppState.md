# AppState

Глобальное axum-state приложения, Clone (содержит Arc/broadcast::Sender).

Поля (актуально на Phase 1):
- projects: Arc<RwLock<ProjectStore>> — реестр проектов под shared lock.
- tasks_tx: broadcast::Sender<TaskEvent> — глобальный канал tasks-watcher событий.
- active_path_tx: Arc<watch::Sender<PathBuf>> — для пересоздания watcher при смене active project.
- attention: Arc<attention::AttentionState> — флаги «session needs attention» от attention::watcher_loop.
- notify: notifier::NotifyHandle — handle к notifier_loop для promote TODO.
- todos: TodoStore — карточки активного проекта.
- todos_tx: broadcast::Sender<TodoEvent> — канал TODO событий.
- themes: Arc<RwLock<ThemesState>> — тема + custom-список.
- themes_dir: PathBuf — каталог themes.json.
- remote_mode: bool — Phase 1: true если запущен с --remote или server_config.json подразумевает remote. Читается healthz; будущие endpoints условно активируются.
- auth_token: Arc<Option<String>> — Phase 1: Bearer-token, ожидаемый middleware. None в legacy localhost (middleware не подключается). Arc для дешёвого Clone.
