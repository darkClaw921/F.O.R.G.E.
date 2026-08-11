# tmux::send_key_target

tmux-web/src/tmux.rs — отправка ОДНОЙ клавиши в КОНКРЕТНУЮ панель сессии, без добавления Enter (бывш. send_key, переименована в tw-8jo).

Зачем отдельно от send_keys: send_keys по контракту дожимает Enter после каждой строки (доставка текста notifier'ом). Пункт меню Claude Code выбирается нажатием цифры, лишний Enter улетел бы в поле ввода пустым сообщением.

Почему панель задаёт вызывающий, а не resolve_target: авто-апрув обязан нажать ровно в ту панель, в которой УВИДЕЛ prompt (capture_pane_target). Автоподбор «лучшей» Claude-панели в сессии с несколькими окнами отправил бы клавишу в другое окно — это был баг tw-8jo.

Сигнатура: send_key_target(session, pane, key), где pane — суффикс 'win.pane' из list_claude_pane_targets; пустая строка = активная панель сессии ('<session>:').

Реализация: валидация имени сессии (is_valid_session_name), пустой key → Ok(()), затем общий send_key_to(target, key): ключи из NAMED_KEYS (Enter, Escape, Space, Tab, BSpace, стрелки, Home, End, C-c, C-d) шлются как именованные клавиши, всё прочее — литералом через '-l --'.
