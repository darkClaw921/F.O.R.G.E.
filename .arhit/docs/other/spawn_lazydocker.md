# spawn_lazydocker

Запускает бинарь lazydocker в указанном cwd через portable-pty и возвращает живой PtyHandle. Используется ws-handler'ом lazydocker_attach в tmux-web/src/ws.rs.

Сигнатура: pub fn spawn_lazydocker(cwd: &Path, cols: u16, rows: u16) -> Result<PtyHandle>

Параметры:
- cwd: рабочий каталог. Lazydocker подключается к локальному docker-daemon вне зависимости от cwd, но cwd обычно совпадает с активным проектом.
- cols/rows: стартовый размер PTY (xterm grid).

Env: TERM=xterm-256color, прочее наследуется от текущего процесса (важно для $HOME/.config/lazydocker/config.yml).

Error handling: при отсутствии бинаря в PATH возвращает Err с подсказкой по установке: 'brew install lazydocker (macOS) | pacman -S lazydocker (Arch) | https://github.com/jesseduffield/lazydocker'.

Источник: tmux-web/src/pty.rs (зеркало spawn_lazygit).
