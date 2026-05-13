# ws::handle_socket

Основной обработчик одного WebSocket-соединения в tmux-web/src/ws.rs (private async fn). Финальная версия после bug-fix forge-qs0.

## Сигнатура

async fn handle_socket(socket: WebSocket, q: AttachQuery)

## Этапы

1. spawn_tmux_attach(session, cols, rows) — если упал, шлём JSON error через socket.send и Close, return.
2. Создаём shared state: pty (Arc<Mutex<Option<PtyHandle>>>), cancel (Arc<AtomicBool>), pty_to_ws_tx/rx (mpsc 64 chunks), pty_eof_notify (Arc<Notify>), reader_handle (Arc<Mutex<Option<JoinHandle>>>), ws_tx/rx (split + Arc<Mutex>).
3. spawn_pty_reader(&pty, tx_clone, cancel, eof_notify) — запускаем reader-task.
4. spawn writer-task: while pty_to_ws_rx.recv() → ws_tx.send(Message::Binary). Завершается, когда все senders дропнуты.
5. Главный loop: tokio::select! biased { _ = pty_eof_notify.notified() => break (PTY died); opt = ws_rx.next() => match Message::Binary | Text | Close | Ping/Pong }.

## Обработка Message::Binary

User input в PTY. Берём pty.lock, writer_mut().write_all(bytes) + flush через tokio::task::spawn_blocking (blocking I/O).

## Обработка Message::Text (JSON Control)

- Resize{cols,rows}: cur_cols/cur_rows update, pty.lock.resize(). Лог warn если resize упал.
- Switch{session}: cancel=true → drop pty (Drop kills tmux child) → await old reader → spawn_tmux_attach(new) → cancel=false, новый Arc::new(Notify) → spawn_pty_reader заново.
- Err: tracing::warn (raw text), WS остаётся живым.

## Teardown

- cancel.store(true).
- *pty.lock = None → Drop → kill+wait tmux.
- await reader_handle.
- drop(pty_to_ws_tx) → writer-task получает None → завершение.
- await writer_task.
- ws_tx.send(Close) + close() best-effort.

## EOF-detection (forge-qs0)

При внешнем 'tmux kill-session' reader-task получает EOF, проверяет !cancel и notify_one() в pty_eof_notify. select!.biased ловит первым и break'ит главный loop. Далее обычный teardown шлёт Message::Close клиенту. Verified: WS закрывается в течение ~1с.
