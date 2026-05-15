# handle_tui_socket

Generic-обработчик WS-соединения для одиночной TUI-сессии (lazygit, lazydocker, tv/television, ...). Извлечён из бывшего handle_lazygit_socket в рамках рефакторинга для поддержки нескольких TUI-вкладок.

Сигнатура:
  async fn handle_tui_socket<F>(socket: WebSocket, q: LazygitQuery, spawn_fn: F, label: &'static str)
  where F: Fn(&Path, u16, u16) -> anyhow::Result<PtyHandle> + Send + Sync + 'static

Параметры:
- socket: уже-апгрейженный axum WebSocket.
- q: LazygitQuery (cwd + cols + rows). Структура общая для всех cwd-ориентированных TUI-табов.
- spawn_fn: фабрика PTY (spawn_lazygit / spawn_lazydocker / spawn_television).
- label: статическая строка для tracing и для error-frame'ов клиенту.

Поведение:
- Создаёт первый PTY через spawn_fn. На ошибку — Text-frame ErrorFrame и Close.
- Запускает spawn_pty_reader → mpsc → WS-writer-task (binary frames).
- Главный loop читает WS: Binary → write в PTY; Text JSON LazygitControl:
    Resize{cols,rows} → SIGWINCH в PTY;
    SwitchCwd{cwd} → kill старый, spawn новый через ту же spawn_fn.
- Teardown: cancel + drop PTY + await reader/writer + Close WS.

Бывший handle_lazygit_socket теперь one-line wrapper:
  handle_tui_socket(socket, q, spawn_lazygit, 'lazygit').await

Источник: tmux-web/src/ws.rs.
