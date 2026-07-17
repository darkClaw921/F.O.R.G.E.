# repo_toplevel

Функция tmux-web/src/worktree.rs. Возвращает абсолютный корень ГЛАВНОГО рабочего дерева (main worktree) git-репозитория, которому принадлежит cwd — ДАЖЕ если cwd находится внутри linked-worktree (.forge-worktrees/<имя>).

Назначение: единый резолвер корня для мутирующих worktree-операций. Фиче всегда нужен ГЛАВНЫЙ корень: там лежит каталог .forge-worktrees/, туда пишется .gitignore, оттуда безопасно выполнять git worktree add/remove.

Почему НЕ 'git rev-parse --show-toplevel': запущенный внутри linked-worktree, --show-toplevel вернул бы корень САМОГО linked-worktree, а не главного дерева. В delete_worktree_window cwd окна — это сам worktree, поэтому --show-toplevel дал бы worktree-корень → worktrees_base указал бы на несуществующий <worktree>/.forge-worktrees → канонизация падала бы с 400 и удаление было бы невозможно (исправленный баг Фазы 4).

Реализация: спавнит 'git rev-parse --path-format=absolute --git-common-dir' через tokio::process::Command с current_dir(cwd). Эта команда возвращает АБСОЛЮТНЫЙ путь общего git-каталога '<main>/.git' — одинаковый для главного дерева и всех linked-worktree. Корень главного дерева = его родитель (Path::parent). Требует git >= 2.31 (--path-format).

Сигнатура: pub async fn repo_toplevel(cwd: &Path) -> anyhow::Result<Option<PathBuf>>.

Параметры:
- cwd: рабочая директория (cwd активной панели сессии в create, cwd конкретного окна в delete).

Возврат:
- Ok(Some(path)): cwd в git-репозитории; path — корень главного рабочего дерева.
- Ok(None): не git-репозиторий (ненулевой exit 'fatal: not a git repository'), git не заспавнился (нет в PATH), пустой вывод, либо у общего git-каталога нет родителя. НИКОГДА не всплывает как Err — вызывающий сам решает, что показать (эндпоинты отдают 400 'сессия/окно не в git-репозитории').

Связи: вызывается в create_worktree_window (cwd активной панели → главный корень; клик 'радужной кнопки' из worktree-окна создаёт копию-СОСЕД под главным репо, а не вложенную) и delete_worktree_window (cwd окна → главный корень → проверка принадлежности .forge-worktrees + git worktree remove). Покрыта unit-тестами: repo_toplevel_returns_none_for_non_repo и repo_toplevel_from_linked_worktree_returns_main_root (регресс: из linked-worktree возвращает главный корень).
