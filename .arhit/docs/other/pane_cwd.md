# pane_cwd

tmux::pane_cwd(session, pane_target) (tmux-web/src/tmux.rs) — cwd КОНКРЕТНОЙ панели через tmux display-message -p -t <session>:<win>.<pane> -F '#{pane_current_path}'. Пустой pane_target даёт таргет <session>: (двоеточие обязательно — иначе для числовых имён сессий tmux возьмёт чужое окно). Аналоги: session_cwd (активная панель сессии), window_cwd (панель окна по индексу). Используется LLM-эскалацией блоклиста авто-апрува: без рабочей директории панели модель считает любой абсолютный путь в команде чужим проектом и отказывает.
