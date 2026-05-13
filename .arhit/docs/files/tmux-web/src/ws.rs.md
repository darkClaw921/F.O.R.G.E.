# tmux-web/src/ws.rs

WebSocket-handlers модуля. Содержит два публичных endpoint'а: /ws/attach (tmux session bridge) и /ws/lazygit (lazygit TUI bridge). Оба переиспользуют общую low-level функцию spawn_pty_reader из этого же модуля.

## Endpoints

### GET /ws/attach?session=<name>&cols=<u16>&rows=<u16>
Апгрейд в WebSocket. Дефолты: cols=80, rows=24. session обязателен (axum Query-extractor вернёт 400 если отсутствует). Spawn'ит tmux attach -t <session> через spawn_tmux_attach.

### GET /ws/lazygit?cwd=<abs-path>&cols=<u16>&rows=<u16>
Апгрейд в WebSocket. cwd обязателен (абсолютный путь к git-репозиторию или к любой папке внутри него — lazygit найдёт ближайший .git). Дефолты: cols=80, rows=24. Spawn'ит lazygit через spawn_lazygit с CWD=cwd. Назначен для xterm.js во фронтенде в git-tab.

## Зачем два отдельных handler'а (а не один параметризованный)

LazygitControl::SwitchCwd { cwd } и Control::Switch { session } — семантически разные операции:
- attach оперирует именами tmux-сессий (короткие, без слешей).
- lazygit оперирует абсолютными путями к проектам.
Объединять в один enum означает либо терять типизацию (String с двойным смыслом), либо плодить варианты — оба плохо. Раздельные handler'ы позволяют независимо эволюционировать (например, добавить read-only flag для lazygit, не задевая attach).

## Wire-протокол (одинаковый для обоих)

Binary frames в обе стороны — сырые байты PTY (escape-последовательности xterm-256color). Browser xterm.js пишет введённые символы как Binary, сервер шлёт stdout PTY как Binary.

Text frames от клиента — JSON control-сообщения. Tag discriminator — поле type.

Close frame — корректный teardown.

## DTO структуры

### AttachQuery (для /ws/attach)
{ session: String, cols: u16 (default 80), rows: u16 (default 24) }

### Control (для /ws/attach, #[serde(tag='type', rename_all='lowercase')])
- {\"type\":\"resize\",\"cols\":120,\"rows\":40} — pty.resize().
- {\"type\":\"switch\",\"session\":\"other\"} — kill старого PTY, spawn нового.

### LazygitQuery (для /ws/lazygit)
{ cwd: String, cols: u16 (default 80), rows: u16 (default 24) }

### LazygitControl (для /ws/lazygit, #[serde(tag='type', rename_all='snake_case')])
- {\"type\":\"resize\",\"cols\":80,\"rows\":24} — pty.resize().
- {\"type\":\"switch_cwd\",\"cwd\":\"/abs/path/to/other/repo\"} — kill старого lazygit, spawn нового в новом cwd.

### ErrorFrame<'a>
Сериализуется как {\"type\":\"error\",\"message\":\"...\"}. Используется при spawn-fail (как при первом upgrade'е, так и при switch_cwd). После отправки сразу шлётся Close frame и WS закрывается. Frontend ожидает этот формат для error-баннера.

## Архитектура handle_socket / handle_lazygit_socket

Обе функции структурно идентичны — отличаются только spawn-функцией и control-enum'ом. Каждая владеет:
- pty: Arc<Mutex<Option<PtyHandle>>> — PTY под mutex (None во время switch swap).
- cancel: Arc<AtomicBool> — сигнал останова reader-task'у (для switch / teardown).
- pty_eof_notify: Arc<Notify> — сигнал от reader-task о EOF/ошибке PTY.
- ws_tx: Arc<Mutex<SplitSink>> — общий для writer-task и error-replies.
- reader_handle: Arc<Mutex<Option<JoinHandle>>> — для await при switch и teardown.

Запускает три tokio-таска:
1. PTY→WS reader-task (spawn_pty_reader): tokio::task::spawn_blocking, синхронно read() из PTY reader (Box<dyn Read+Send>) в буфер 8 KiB (READ_BUF), отправляет Vec<u8> через mpsc::channel(64). При EOF/error и cancel=false — notify_one() в pty_eof_notify.
2. WS-writer task: получает чанки из mpsc::Receiver, шлёт ws_tx.send(Message::Binary(chunk)). Не проверяет cancel-флаг — живёт пока есть senders.
3. Главный future: tokio::select! на ws_rx.next() и pty_eof_notify.notified() (BIASED — eof проверяется первым).

## Switch flow (Switch / SwitchCwd)

1. cancel.store(true).
2. *pty.lock = None → Drop PtyHandle → kill+wait child → reader получит EOF.
3. await старого reader-task'а.
4. spawn_<tmux_attach|lazygit>(new_target, cur_cols, cur_rows). Если упал — JSON ErrorFrame и break (закрытие WS).
5. cancel.store(false), pty_eof_notify = Arc::new(Notify::new()) (новый Notify, чтобы EOF старого reader не разбудил handler), spawn_pty_reader снова с тем же mpsc::Sender clone.

## Teardown (after main loop break)

1. cancel.store(true).
2. *pty.lock = None → Drop → kill+wait.
3. await reader_handle.
4. drop(pty_to_ws_tx) → writer-task получает None → завершение.
5. await writer_task.
6. ws_tx.send(Close) + close() (best-effort).

## spawn_pty_reader (общая функция)

Принимает &Arc<Mutex<Option<PtyHandle>>>, mpsc::Sender, cancel-AtomicBool, eof-Notify. Берёт reader из handle (take_reader, one-shot), spawn_blocking-loop читает READ_BUF=8 KiB, blocking_send в mpsc. Завершается на EOF/ошибке/cancel. notify_one() только если natural_death=true И cancel=false (не сигнализит при switch/teardown).

## Bug-fix forge-qs0 (Phase 5, attach)

До фикса: при внешнем kill-session reader-task детектировал EOF и завершался, но главный handle_socket loop продолжал ждать ws_rx.next() — клиент не получал Close. Решение: добавлен tokio::sync::Notify, на EOF (когда cancel=false) reader сигналит через notify_one(). lazygit-handler наследует это поведение через ту же spawn_pty_reader.

## Константы

- READ_BUF = 8 KiB — размер чанка чтения из PTY.
- CHAN_DEPTH = 64 — глубина mpsc-канала PTY→WS (~0.5 MiB буферизации).

## Регистрация в main.rs

- .route('/ws/attach', get(ws::attach))
- .route('/ws/lazygit', get(ws::lazygit_attach))

## Smoke-test (Phase 2, forge-nbl.4)

curl-проверки:
- GET без Upgrade → 400 'Connection header did not include upgrade'.
- GET ?cwd=...&Upgrade → 101 Switching Protocols + (если lazygit отсутствует) Text-frame {\"type\":\"error\",\"message\":\"spawn failed: failed to spawn lazygit in '/path': lazygit not found in PATH, install via brew install lazygit (macOS) or your distro's package manager\"} + Close.
- Missing cwd query → 400.
