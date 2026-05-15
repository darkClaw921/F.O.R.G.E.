# tmux-web/src/ws.rs

WebSocket-handlers модуля. Содержит четыре публичных endpoint'а: /ws/attach (tmux session bridge), /ws/lazygit, /ws/lazydocker и /ws/telescope (все три — TUI bridges).

## Публичные handler-функции

- pub async fn attach(...) — GET /ws/attach. Spawn'ит tmux attach через spawn_tmux_attach.
- pub async fn lazygit_attach(...) — GET /ws/lazygit. Spawn'ит lazygit через spawn_lazygit.
- pub async fn lazydocker_attach(...) — GET /ws/lazydocker. Spawn'ит lazydocker через spawn_lazydocker (Phase 1, forge-ddyl).
- pub async fn telescope_attach(...) — GET /ws/telescope. Spawn'ит tv через spawn_television (Phase 1, forge-ddyl).

Все три TUI-handler'а имеют идентичную структуру:
1. Парсинг ?server=<id> для remote-mode → remote_proxy::proxy_websocket с upstream_path соответствующего endpoint'а.
2. parse_lazygit_query (общий: cwd + cols + rows).
3. ws.on_upgrade → handle_tui_socket(socket, q, spawn_fn, label).

## Generic-функция handle_tui_socket<F>

Phase 1 рефакторинг: бывший handle_lazygit_socket переименован и сделан generic. Принимает spawn-фабрику F: Fn(&Path, u16, u16) -> Result<PtyHandle> + Send + Sync + 'static и метку label для tracing.

Старая функция handle_lazygit_socket теперь one-line wrapper: handle_tui_socket(socket, q, spawn_lazygit, 'lazygit').await.

## Wire-протокол (общий для всех 4 endpoint'ов)

- Binary frames в обе стороны = сырые байты PTY (xterm-256color).
- Text frames от клиента = JSON control-сообщения (tag=type).
- Error frames от сервера: {\"type\":\"error\",\"message\":\"...\"} (см. ErrorFrame), сразу за ним Close.

## DTO структуры

### AttachQuery (для /ws/attach)
{ session: String, cols: u16 (default 80), rows: u16 (default 24) }

### Control (для /ws/attach, #[serde(tag='type', rename_all='lowercase')])
- {\"type\":\"resize\",\"cols\":120,\"rows\":40} → pty.resize().
- {\"type\":\"switch\",\"session\":\"other\"} → kill старого, spawn нового.

### LazygitQuery (используется ВСЕМИ TUI: lazygit/lazydocker/telescope)
{ cwd: String, cols: u16 (default 80), rows: u16 (default 24) }
Имя сохранено для обратной совместимости.

### LazygitControl (используется ВСЕМИ TUI: #[serde(tag='type', rename_all='snake_case')])
- {\"type\":\"resize\",\"cols\":80,\"rows\":24} → pty.resize().
- {\"type\":\"switch_cwd\",\"cwd\":\"/abs/path\"} → kill старого PTY, spawn нового в новом cwd через ту же spawn_fn.

### ErrorFrame<'a>
Сериализуется как {\"type\":\"error\",\"message\":\"...\"}. Используется при spawn-fail (как при первом upgrade'е, так и при switch_cwd). После отправки сразу шлётся Close frame.

## Архитектура handle_socket / handle_tui_socket

Обе функции структурно идентичны — отличаются только spawn-источником и control-enum'ом. Каждая владеет:
- pty: Arc<Mutex<Option<PtyHandle>>> — PTY под mutex (None во время switch swap).
- cancel: Arc<AtomicBool> — сигнал останова reader-task'у (для switch / teardown).
- pty_eof_notify: Arc<Notify> — сигнал от reader-task о EOF/ошибке PTY.
- ws_tx: Arc<Mutex<SplitSink>> — общий для writer-task и error-replies.
- reader_handle: Arc<Mutex<Option<JoinHandle>>> — для await при switch и teardown.

Запускает три tokio-таска:
1. PTY→WS reader-task (spawn_pty_reader): spawn_blocking, синхронный read() из PTY reader (8 KiB), отправка Vec<u8> через mpsc::channel(64). При EOF/error и cancel=false — notify_one().
2. WS-writer task: получает чанки из mpsc::Receiver, шлёт ws_tx.send(Message::Binary(chunk)).
3. Главный future: tokio::select! на ws_rx.next() и pty_eof_notify.notified() (BIASED — eof первым).

## Switch flow (Switch / SwitchCwd)

1. cancel.store(true).
2. *pty.lock = None → Drop PtyHandle → kill+wait child → reader получит EOF.
3. await старого reader-task'а.
4. spawn_fn(new_target, cur_cols, cur_rows). Если упал — JSON ErrorFrame и break (закрытие WS).
5. cancel.store(false), новый Notify (чтобы EOF старого reader не разбудил handler), spawn_pty_reader снова.

## Teardown

1. cancel.store(true). 2. drop PTY. 3. await reader_handle. 4. drop sender → writer-task завершается. 5. ws_tx.send(Close) + close().

## spawn_pty_reader (общая)

Принимает &Arc<Mutex<Option<PtyHandle>>>, mpsc::Sender, cancel-AtomicBool, eof-Notify. Берёт reader из handle (take_reader), spawn_blocking-loop читает READ_BUF=8 KiB. Завершается на EOF/ошибке/cancel. notify_one() только если natural_death=true И cancel=false.

## Bug-fix forge-qs0 (Phase 5, attach)

Добавлен tokio::sync::Notify для надёжного teardown при внешнем kill-session: на EOF (когда cancel=false) reader сигналит через notify_one(). Все TUI-handler'ы (lazygit/lazydocker/telescope) наследуют это через handle_tui_socket.

## remote_proxy интеграция

Все WS-эндпоинты поддерживают ?server=<id> в remote-mode. lazydocker_attach/telescope_attach используют upstream_path '/ws/lazydocker' и '/ws/telescope' соответственно при проксировании на удалённый сервер.

## Константы

- READ_BUF = 8 KiB.
- CHAN_DEPTH = 64.

## Регистрация в main.rs

- .route('/ws/attach', get(ws::attach))
- .route('/ws/lazygit', get(ws::lazygit_attach))
- .route('/ws/lazydocker', get(ws::lazydocker_attach))
- .route('/ws/telescope', get(ws::telescope_attach))
