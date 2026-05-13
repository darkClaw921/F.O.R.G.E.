# tmux-web/src/tmux.rs::capture_pane

Захватывает только видимую часть активной панели tmux-сессии (без scrollback). Команда: tmux capture-pane -p -t <session>. Используется attention::watcher_loop для детекции Claude permission prompt. Раньше использовался флаг -S -30 (последние 30 строк history), что давало false-positive: старый prompt из scrollback продолжал срабатывать после ответа юзера. Гонка list_sessions ↔ capture_pane обработана: 'no server running' и 'can't find session' в stderr → Ok(String::new()), watcher продолжает loop. Прочие сбои tmux → Err.
