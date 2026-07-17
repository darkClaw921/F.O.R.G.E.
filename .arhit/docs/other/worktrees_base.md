# worktrees_base

Функция tmux-web/src/worktree.rs. Возвращает каталог-контейнер рабочих копий для данного toplevel: <toplevel>/.forge-worktrees.

Назначение: единая точка формирования пути к скрытому каталогу-контейнеру, чтобы имя каталога (константа WORKTREES_DIR='.forge-worktrees') не дублировалось в вызывающем коде. Возвращаемый путь — база, которую ожидает alloc_worktree_name и которую скрывает ensure_gitignore_entry.

Сигнатура: pub fn worktrees_base(toplevel: &Path) -> PathBuf. Реализация: toplevel.join(WORKTREES_DIR).

Параметры:
- toplevel: корень репозитория (обычно результат repo_toplevel).

Связи: используется в create_worktree_window (как база для alloc_worktree_name) и в delete_worktree_window (канонизируется и служит границей проверки безопасности — cwd удаляемого окна обязан лежать ПОД этим каталогом). Покрыта unit-тестом (worktrees_base('/proj') == '/proj/.forge-worktrees').
