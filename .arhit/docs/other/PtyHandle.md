# PtyHandle

Дескриптор живого PTY (псевдотерминала) с запущенной командой tmux attach. Расположен в tmux-web/src/pty.rs.

## Поля
- master: Box<dyn MasterPty + Send> — master-сторона псевдотерминала; на ней вызывается resize().
- child: Option<Box<dyn Child + Send + Sync>> — дочерний процесс tmux. Хранится в Option, чтобы Drop мог взять владение.
- reader: Option<Box<dyn Read + Send>> — blocking-reader stdout PTY.
- writer: Option<Box<dyn Write + Send>> — blocking-writer stdin PTY.

## Методы
- resize(cols, rows) -> Result<()> — отправляет SIGWINCH через master.resize(). pixel_width/height нулевые.
- take_reader() -> Option<Box<dyn Read+Send>> — one-shot, забирает reader для перемещения в spawn_blocking.
- take_writer() -> Option<Box<dyn Write+Send>> — аналогично writer.
- child_pid() -> Option<u32> — PID tmux child, если он жив.

## Drop
Гарантированно kill+wait для child — без зомби. Ошибки игнорируются (Drop не паникует).

## Thread-safety
MasterPty/Child — Send но не обязательно Sync. Reader/writer — sync (std::io::Read/Write), поэтому в ws.rs они оборачиваются в spawn_blocking. Master и child живут в async-таске владельца PtyHandle.
