# spawn_television

Запускает бинарь tv (television, fuzzy finder) в указанном cwd через portable-pty и возвращает живой PtyHandle. Используется ws-handler'ом telescope_attach в tmux-web/src/ws.rs.

Сигнатура: pub fn spawn_television(cwd: &Path, cols: u16, rows: u16) -> Result<PtyHandle>

Параметры:
- cwd: рабочий каталог — корень поиска для tv (файлы, директории, git-history).
- cols/rows: стартовый размер PTY.

Env: TERM=xterm-256color, прочее наследуется (важно для $HOME/.config/television/config.toml).

Error handling: при отсутствии бинаря 'tv' в PATH — Err с подсказкой: 'brew install television (macOS) | cargo install television | https://github.com/alexpasmantier/television'.

Источник: tmux-web/src/pty.rs (зеркало spawn_lazygit).
