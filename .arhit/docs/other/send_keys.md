# send_keys

Доставка текста в tmux-сессию с нажатием Enter (tmux-web/src/tmux.rs). Сигнатура: pub async fn send_keys(session: &str, text: &str) -> anyhow::Result<()>. Используется notifier::fire_job для доставки текста промоутнутого TODO (ручной промоут и авто-цепочка auto_promote).

Поведение:
1. Валидация имени сессии через is_valid_session_name ([A-Za-z0-9_-]+).
2. Пустой text -> Ok(()) без действий.
3. Резолв target через find_claude_pane: текст идёт в панель с запущенным Claude CLI ('<session>:<win>.<pane>'), в каком бы окне она ни была. Фолбэк (Claude-панель не найдена или list-panes упал): '<session>:' — активная панель сессии; ':' обязателен для числовых имён сессий. Фикс forge-v6pw: раньше target был голым именем сессии -> доставка в активное окно -> при нескольких окнах текст попадал в шелл вместо Claude, и авто-цепочка «не запускалась».
4. Многострочный text шлётся построчно: send-keys -t <target> -l <line> + отдельный send-keys -t <target> Enter. Без shell-интерпретации (Command::args).

Ошибки: invalid session name -> bail; 'no server running'/'can't find session' НЕ глушатся (caller обязан узнать о провале доставки — notifier ретраит x3 с backoff 500/1000/2000ms).

Связанные: find_claude_pane, pick_claude_pane, is_claude_command, notifier::fire_job, promote_todo_core, auto_promote::handle_closed.
