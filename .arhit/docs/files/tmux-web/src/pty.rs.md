# tmux-web/src/pty.rs

Модуль PTY-обёрток над portable-pty (0.8) для запуска интерактивных TUI-программ внутри псевдотерминалов. Используется ws.rs для bridge'а между WebSocket и TUI-процессом.

## Spawn-функции

- pub fn spawn_tmux_attach(session: &str, cols: u16, rows: u16) — 'tmux attach -t <session>'.
- pub fn spawn_lazygit(cwd: &Path, cols: u16, rows: u16) — 'lazygit' в cwd.
- pub fn spawn_lazydocker(cwd: &Path, cols: u16, rows: u16) — 'lazydocker' в cwd (Phase 1, forge-ddyl).
- pub fn spawn_television(cwd: &Path, cols: u16, rows: u16) — 'tv' в cwd (Phase 1, forge-ddyl).

Все четыре возвращают anyhow::Result<PtyHandle> и имеют идентичную структуру: native_pty_system().openpty → CommandBuilder с TERM=xterm-256color и опциональным cwd → pair.slave.spawn_command (с осмысленным контекстом ошибки) → drop(pair.slave) → master.try_clone_reader/take_writer → PtyHandle{...}.

Все три TUI spawn-функции (spawn_lazygit/spawn_lazydocker/spawn_television) используются в generic handle_tui_socket<F> в ws.rs через переданную spawn-фабрику. Это позволяет добавлять новые TUI-вкладки без копирования handler-логики.

## Структура PtyHandle

pub struct PtyHandle {
  master: Box<dyn MasterPty + Send>,           // master-сторона PTY (вызов resize)
  child: Option<Box<dyn Child + Send + Sync>>, // дочерний процесс TUI (Option для Drop)
  reader: Option<Box<dyn Read + Send>>,        // blocking-reader stdout PTY
  writer: Option<Box<dyn Write + Send>>,       // blocking-writer stdin PTY
}

Универсален для tmux/lazygit/lazydocker/tv — структура одна, отличаются только spawn-функции.

## Инварианты

- master жив всё время существования handle (resize и owner-мастер).
- child хранится в Option, чтобы Drop мог взять владение и сделать kill+wait без unsafe.
- reader/writer — Option (one-shot move в spawn_blocking).
- Drop kill'ит и reap'ает ребёнка — гарантия отсутствия зомби.

## Методы

- pub fn resize(cols, rows) -> anyhow::Result<()> — SIGWINCH через master.resize() с PtySize{rows, cols, pixel_width:0, pixel_height:0}.
- pub fn take_reader() / take_writer() — one-shot забор endpoint'а в spawn_blocking.
- pub fn writer_mut(&mut self) -> Option<&mut Box<dyn Write+Send>> — мут. ссылка без забора владения. Используется ws.rs для записи user input под Mutex<PtyHandle>.
- pub fn child_pid(&self) -> Option<u32> — PID дочернего процесса.

## Drop

Гарантированно kill+wait для дочернего процесса. Ошибки игнорируются. Критично: WS-handler полагается на Drop при teardown / switch_cwd / switch.

## Подсказки по установке в error-обёртках

Каждая spawn-функция при отсутствии бинаря в PATH оборачивает ошибку с подсказкой по установке (видна пользователю в error-banner фронтенда):
- spawn_lazygit: 'brew install lazygit (macOS) or your distro's package manager'.
- spawn_lazydocker: 'brew install lazydocker (macOS) | pacman -S lazydocker (Arch) | https://github.com/jesseduffield/lazydocker'.
- spawn_television: 'brew install television (macOS) | cargo install television | https://github.com/alexpasmantier/television'.

Frontend (createTuiTab) дополнительно детектирует binary-not-found по эвристике (имя бинаря + 'not found' в message) и подменяет message на installHelp.notFoundMsg + раскрывает install-help со списком команд per-OS.

## Окружение

Все spawn-функции устанавливают TERM=xterm-256color (обязательно для корректной отрисовки TUI). Остальные env наследуются — важно для /Users/igorgerasimov/.config/{lazygit,lazydocker,television}/config.*.

## Зависимости

- portable-pty 0.8 — кросс-платформенный PTY (openpty, CommandBuilder, MasterPty, Child, PtySize, native_pty_system).
- anyhow — Context/Result для информативных ошибок.
- std::io::{Read, Write} — blocking endpoints.
- std::path::Path — для cwd-аргумента в TUI spawn-функциях.

## Thread-safety

MasterPty/Child — Send, но не Sync. Reader/writer — sync (std::io::Read/Write), оборачиваются в spawn_blocking в ws.rs. Master и child живут в async-таске владельца PtyHandle (в Mutex).
