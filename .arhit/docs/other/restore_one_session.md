# restore_one_session

Приватный async-хелпер в tmux-web/src/main.rs. Восстанавливает одну сессию из истории: tmux::new_session(name, path), затем воссоздаёт окна из записи HistoryStore — окно index 0 переименовывается через tmux::rename_window, остальные создаются tmux::new_window(Some(name)). Ошибки отдельных окон логируются (warn) и не фатальны; Err только при сбое создания сессии. Переиспользуется restore_session_history и restore_all_session_history.
