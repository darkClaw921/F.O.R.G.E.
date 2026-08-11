# tmux::PaneTarget

tmux-web/src/tmux.rs — панель сессии, пригодная для опроса авто-апрувом. Возвращается list_claude_pane_targets / collect_claude_panes.

Поля:
- target: String — суффикс 'win.pane' для '-t <session>:<target>' (capture_pane_target / send_key_target). Пустая строка = активная панель сессии.
- window: String — ИМЯ окна (#{window_name}), то, что человек видит в статусбаре tmux. Идёт в историю апрувов колонкой «Окно»: 'claude-agent' понятнее, чем индекс '1.0'. Может быть пустым, если tmux не отдал имя (старый формат из 4 полей) — тогда UI показывает индекс.

LP_FORMAT расширен пятым полем #{window_name}; оно добавлено ПОСЛЕДНИМ, поэтому pick_claude_pane (читает первые 4 поля) не затронут, а разбор строк без пятого поля остаётся валидным (тест collect_claude_panes_tolerates_missing_window_name).

Фолбэк collect_claude_panes при неопознанном Claude: одна запись с пустым target и именем АКТИВНОГО окна — история остаётся читаемой даже без адреса панели.
