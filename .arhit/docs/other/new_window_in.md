# new_window_in

Функция tmux-web/src/tmux.rs. Создаёт новое tmux-окно с ЯВНО заданной рабочей директорией.

Назначение: аналог tmux::new_window, но cwd задаётся явным путём через '-c <cwd>', а не наследуется от #{pane_current_path} активной панели. Нужна для worktree-окон: окно должно открыться СРАЗУ в каталоге рабочей копии (.forge-worktrees/wt-<ts>), а не там, где стоит активная панель сессии.

Сигнатура: pub async fn new_window_in(session: &str, name: Option<&str>, cwd: &str) -> anyhow::Result<()>.
Команда: 'tmux new-window -t <session>: -c <cwd> [-n <name>]'.

Параметры:
- session: имя сессии; валидируется is_valid_session_name (иначе bail! без spawn).
- name: имя окна — при Some(непустое) добавляется '-n <name>'; при None или пустой строке имя не задаётся (tmux назовёт окно сам).
- cwd: рабочая директория нового окна ('-c <cwd>').

Особенность таргета: '<session>:' с ДВОЕТОЧИЕМ на конце — session-target, чтобы tmux назначил следующий свободный индекс, а не пересоздавал существующее окно (та же логика, что в new_window).

Возврат: Ok(()) при успехе; Err при невалидном имени сессии (без spawn) или ненулевом exit tmux (с обрезанным stderr).

Связи: вызывается из create_worktree_window с name='wt:<ts>' и cwd=путь рабочей копии. При провале вызывающий откатывает worktree (worktree_remove force).
