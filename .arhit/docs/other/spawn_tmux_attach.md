# spawn_tmux_attach

Функция в tmux-web/src/pty.rs. Спавнит tmux attach -t <session> внутри нового PTY размера cols×rows.

## Сигнатура
pub fn spawn_tmux_attach(session: &str, cols: u16, rows: u16) -> anyhow::Result<PtyHandle>

## Алгоритм
1. native_pty_system().openpty(PtySize{rows, cols, pixel_width:0, pixel_height:0}).
2. CommandBuilder::new('tmux') + args(['attach','-t',session]) + env('TERM','xterm-256color').
3. pair.slave.spawn_command(cmd) → Box<dyn Child+Send+Sync>.
4. drop(pair.slave) — закрываем slave-fd в нашем процессе, иначе master не получит EOF после exit ребёнка.
5. master.try_clone_reader() и master.take_writer().
6. Возвращаем PtyHandle{master, child, reader, writer}.

## Окружение
TERM=xterm-256color (без него tmux ушёл бы в degraded). Остальные переменные не наследуются автоматически (CommandBuilder default), но в данной реализации мы их не передаём — для tmux attach критично TERM.

## Ошибки
- openpty failed — система отказала в выделении PTY.
- spawn_command failed — tmux не установлен или PATH пуст.
- try_clone_reader/take_writer — теоретически могут упасть, обёрнуты в context.

## Поведение для несуществующей сессии
PTY всё равно откроется, tmux напишет ошибку в stdout и быстро завершится. ws.rs детектирует это по EOF на reader.
