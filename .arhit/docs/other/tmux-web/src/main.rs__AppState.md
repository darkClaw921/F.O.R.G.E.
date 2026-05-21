# tmux-web/src/main.rs::AppState

Глобальное состояние axum-приложения. Cheap-clonable (все поля Arc/Sender/handle внутри). После remove-projects-concept (Phase 4) поле projects удалено.

Поля:
- tasks_tx: broadcast::Sender<TaskEvent> — tasks_watcher пушит сюда события при изменениях .beads/issues.jsonl. WS /ws/tasks подписывается через subscribe() на каждое соединение.
- active_path_tx: Arc<watch::Sender<PathBuf>> — watch-sender для пересоздания tasks_watcher при смене активной сессии. Инициализирован std::env::current_dir() процесса. Любой эндпоинт, меняющий активный путь (например, ?path= в WS /ws/tasks или /ws/todos через resolve_active_path), может опционально отправить новый path для пере-watch.
- attention: Arc<attention::AttentionState> — shared state needs_attention/is_generating флагов. attention::watcher_loop пишет каждые 1.5с; get_sessions читает snapshot.
- notify: notifier::NotifyHandle — handle к notifier_loop (для promote TODO через POST /api/todos/:id/promote).
- todos: TodoStore — TODO-карточки, сгруппированные по root_path (cwd-derived через paths::resolve_root). Хранение в ~/.config/forge/todos.json.
- todos_tx: broadcast::Sender<TodoEvent> — канал TODO событий для /ws/todos.
- themes: Arc<RwLock<ThemesState>> — глобальные темы.
- themes_dir: PathBuf — каталог themes.json.
- remote_mode: bool — true если запущен с --remote или server_config.json в remote-режиме.
- auth_token: Arc<Option<String>> — Bearer-token для auth middleware. None в localhost-режиме.
- remotes: Arc<RwLock<RemotesStore>> — реестр remote-серверов (только в remote_mode).
- http: reqwest::Client — для remote-proxy.
- user_settings: UserSettingsStore — глобальные настройки пользователя.
- notifier_config: NotifierConfigStore — глобальные настройки notify (template/delay/wait_previous/session). Phase 3: переехало с per-project на один глобальный конфиг ~/.config/forge/notifier.json.

Передаётся в handlers через axum::extract::State<AppState>.
