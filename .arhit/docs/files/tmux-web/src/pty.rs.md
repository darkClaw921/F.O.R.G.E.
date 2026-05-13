# tmux-web/src/pty.rs

Модуль PTY-обёрток над portable-pty (0.8) для запуска интерактивных TUI-программ внутри псевдотерминалов: tmux attach (spawn_tmux_attach) и lazygit (spawn_lazygit). Используется ws.rs для bridge'а между WebSocket и TUI-процессом.

## Структура PtyHandle

pub struct PtyHandle {
  master: Box<dyn MasterPty + Send>,           // master-сторона PTY (вызов resize)
  child: Option<Box<dyn Child + Send + Sync>>, // дочерний процесс TUI (Option для Drop)
  reader: Option<Box<dyn Read + Send>>,        // blocking-reader stdout PTY
  writer: Option<Box<dyn Write + Send>>,       // blocking-writer stdin PTY
}

PtyHandle универсален: одна и та же структура используется для tmux и для lazygit. Различие — только в spawn-функции, которая создаёт handle.

Реализована обёртка над portable-pty 0.8 (нативный posix_openpt/grantpt/unlockpt на macOS, ConPTY/winpty на Windows).

## Инварианты PtyHandle

- master жив всё время существования handle (resize и owner-мастер).
- child хранится в Option, чтобы Drop мог взять владение и сделать kill+wait без unsafe.
- reader/writer — Option, поскольку take_reader/take_writer — one-shot операции, перемещающие endpoint в spawn_blocking-таску.
- Drop kill'ит и reap'ает ребёнка — гарантия отсутствия зомби.

## Методы

- pub fn resize(cols, rows) -> anyhow::Result<()> — отправляет SIGWINCH через master.resize() с PtySize { rows, cols, pixel_width:0, pixel_height:0 }.
- pub fn take_reader() -> Option<Box<dyn Read+Send>> — one-shot: забирает reader для перемещения в spawn_blocking (ws::spawn_pty_reader).
- pub fn take_writer() -> Option<Box<dyn Write+Send>> — аналогично writer.
- pub fn writer_mut(&mut self) -> Option<&mut Box<dyn Write+Send>> — возвращает мутабельную ссылку на writer без забора владения. Используется ws.rs для записи user input в PTY под Mutex<PtyHandle>.
- pub fn child_pid(&self) -> Option<u32> — PID дочернего процесса, если он жив.

## Drop

Гарантированно kill+wait для дочернего процесса, чтобы не оставить зомби. Ошибки игнорируются (Drop не паникует). Это критично: WS-handler полагается на Drop при teardown / switch.

## pub fn spawn_tmux_attach(session: &str, cols: u16, rows: u16) -> anyhow::Result<PtyHandle>

Спавнит 'tmux attach -t <session>' внутри нового PTY размера cols×rows.

Алгоритм:
1. native_pty_system().openpty(PtySize{rows, cols, pixel_width:0, pixel_height:0}).
2. CommandBuilder::new('tmux') с args(['attach','-t',session]) + env('TERM','xterm-256color').
3. pair.slave.spawn_command(cmd) → Box<dyn Child+Send+Sync>.
4. drop(pair.slave) — закрываем slave-fd в нашем процессе, иначе master не получит EOF после exit ребёнка.
5. master.try_clone_reader() и master.take_writer().
6. Возвращает PtyHandle{master, child, reader, writer}.

Окружение: TERM=xterm-256color — обязательно, без него tmux ушёл бы в degraded mode.

Ошибки:
- openpty failed — система отказала в выделении PTY.
- failed to spawn 'tmux attach -t <session>' — tmux не установлен или PATH пуст.

Поведение для несуществующей сессии: PTY всё равно откроется, tmux напишет 'no session ...' в stdout и завершится — ws.rs детектирует это по EOF на reader.

## pub fn spawn_lazygit(cwd: &Path, cols: u16, rows: u16) -> anyhow::Result<PtyHandle>

Спавнит 'lazygit' внутри нового PTY размера cols×rows с заданным рабочим каталогом cwd. Используется в WebSocket-handler'е /ws/lazygit (Phase 2) для интерактивного git-UI прямо в браузере (xterm.js фронтенд).

Сигнатура и стиль идентичны spawn_tmux_attach. Различие:
- CommandBuilder::new('lazygit') без args (lazygit ищет ближайший .git вверх по дереву от cwd).
- cmd.cwd(cwd) — явно задаёт рабочий каталог (обычно путь активного проекта из projects.rs).
- with_context-сообщение содержит подсказку: 'lazygit not found in PATH, install via brew install lazygit (macOS) or your distro's package manager'.

Алгоритм:
1. native_pty_system().openpty(PtySize{rows, cols, pixel_width:0, pixel_height:0}).
2. CommandBuilder::new('lazygit') + cmd.cwd(cwd) + cmd.env('TERM','xterm-256color').
3. pair.slave.spawn_command(cmd) с обёрткой ошибки в подсказку об установке.
4. drop(pair.slave).
5. master.try_clone_reader() и master.take_writer().
6. Возвращает PtyHandle{master, child, reader, writer}.

Окружение: TERM=xterm-256color — обязательно для корректной отрисовки TUI lazygit (цвета, бордеры). Остальные env наследуются — это важно для $HOME/.config/lazygit/config.yml.

Ошибки:
- openpty failed — система отказала в выделении PTY.
- failed to spawn 'lazygit' in <cwd>: lazygit not found in PATH, install via brew install lazygit ... — обёртка с осмысленной подсказкой пользователю. Phase 2 ws-handler покажет это сообщение в error-banner фронтенда.

EOF на reader сигнализирует, что lazygit завершился (например, пользователь нажал q).

## Зависимости

- portable-pty 0.8 — кросс-платформенный PTY (используется openpty, CommandBuilder, MasterPty, Child, PtySize, native_pty_system).
- anyhow — Context/Result для информативных ошибок.
- std::io::{Read, Write} — blocking endpoints.
- std::path::Path — для параметра cwd в spawn_lazygit.

## Thread-safety

MasterPty/Child — Send, но не обязательно Sync. Reader/writer — sync (std::io::Read/Write), поэтому в ws.rs они обернуты в spawn_blocking. Master и child живут в async-таске владельца PtyHandle (в Mutex).
