# tmux::list_claude_pane_targets

tmux-web/src/tmux.rs — перечисляет панели сессии, в которых имеет смысл искать prompt Claude Code. Возвращает список target-суффиксов 'win.pane' для capture_pane_target/send_key_target.

Выполняет 'tmux list-panes -s -t <session>: -F <LP_FORMAT>' и отдаёт результат чистой функции collect_claude_panes: берутся ВСЕ панели, чей pane_current_command проходит is_claude_command (литеральное 'claude' либо version-like строка вида 2.1.172). Если Claude-панелей нет — возвращается vec![""], то есть один пустой суффикс = активная панель сессии (тот же фолбэк, что у resolve_target: Claude может не опознаться при нестандартной установке, и терять сессию целиком нельзя).

Отличие от find_claude_pane/pick_claude_pane: те выбирают ОДНУ «лучшую» панель (приоритет активному окну), а здесь нужны все — prompt может висеть в фоновом окне, пока пользователь смотрит соседнее.

Исчезнувшая сессия / незапущенный сервер → Ok(vec![]) (нечего обходить); прочие сбои tmux → Err.
