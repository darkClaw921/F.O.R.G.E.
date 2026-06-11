# find_claude_pane

Поиск панели с запущенным Claude Code CLI внутри tmux-сессии (tmux-web/src/tmux.rs). Сигнатура: pub async fn find_claude_pane(session: &str) -> anyhow::Result<Option<String>>.

Назначение: фикс бага forge-v6pw — notify при промоуте TODO->task доставлялся через send-keys -t <session>, т.е. в АКТИВНОЕ окно сессии. При нескольких окнах активным могло быть окно с шеллом, и текст не доходил до Claude (симптом: «отправляется только если в сессии одно окно»). Также из-за этого «не запускалась» следующая задача авто-цепочки: auto_promote::run промоутил карточку, но Immediate-notify уходил в чужое окно.

Алгоритм:
1. tmux list-panes -s -t '<session>:' -F LP_FORMAT (LP_FORMAT = '#{window_index}.#{pane_index}|#{pane_current_command}|#{window_active}|#{pane_active}'). Суффикс ':' обязателен — session-target, иначе числовые имена сессий ('8') резолвятся как окно чужой сессии (тот же баг, что был у capture_pane).
2. pick_claude_pane (чистая функция, юнит-тесты) фильтрует панели по is_claude_command и выбирает лучшую: активная панель активного окна > активное окно > первая по листингу. Возвращает суффикс 'win.pane'.
3. is_claude_command(cmd): true для буквального 'claude' (case-insensitive) ИЛИ version-like строки (только ASCII-цифры и точки, минимум одна точка) — Claude Code переименовывает свой процесс в строку версии ('2.1.172'). 'python3.11', 'zsh', 'node', '123' не проходят.

Возврат: Ok(Some('win.pane')) — найдена; Ok(None) — Claude-панели нет / сессия исчезла / tmux-сервер не запущен; Err — прочие сбои tmux.

Используется: send_keys (резолв target перед доставкой). Связанные: pick_claude_pane, is_claude_command, LP_FORMAT, send_keys, capture_pane (источник паттерна ':'-суффикса).
