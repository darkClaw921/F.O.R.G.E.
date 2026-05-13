# tmux-web/src/tmux.rs::SessionInfo

Метаданные одной tmux-сессии для отдачи во фронтенд. Поля: name (#{session_name}), id (#{session_id}, вида $0), attached (число клиентов), windows, created (unix-таймстамп), path (#{session_path}, стартовый cwd), session_group (Option<String>: #{session_group}, имя tmux session-group для linked-сессий; пустая строка из tmux мапится в None). Derive: Debug+Clone+Serialize+Deserialize+PartialEq+Eq. Поле session_group используется в attention::watcher_loop для дедупликации needs_attention между linked-сессиями.
