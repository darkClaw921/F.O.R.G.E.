# worktree_add

Функция tmux-web/src/worktree.rs. Создаёт новую изолированную рабочую копию на новой ветке.

Назначение: реализует основную мутацию 'радужной кнопки' — 'git worktree add <path> -b <branch>'. git создаёт каталог path, чекаутит в него НОВУЮ ветку branch (флаг -b) и регистрирует worktree в репозитории.

Сигнатура: pub async fn worktree_add(toplevel: &Path, path: &Path, branch: &str) -> anyhow::Result<()>.
Реализация: tokio::process::Command 'git worktree add <path> -b <branch>' с current_dir(toplevel).

Параметры:
- toplevel: корень репозитория (рабочая директория для git; результат repo_toplevel).
- path: путь будущей рабочей копии, обычно <toplevel>/.forge-worktrees/wt-<ts> из alloc_worktree_name. Каталог НЕ должен существовать заранее — его создаёт git.
- branch: имя новой ветки (обычно forge/wt-<ts>). Ветка НЕ должна существовать, иначе -b вернёт ошибку.

Возврат: Ok(()) при успехе. При ненулевом exit git — Err с кодом выхода и обрезанным stderr git (например 'branch already exists' или 'directory already exists'). Spawn-ошибка оборачивается через anyhow::Context.

Ограничения/связи: мутирующая операция (в отличие от read-only crate::git). Вызывается из create_worktree_window; при последующем провале открытия tmux-окна выполняется откат через worktree_remove(force=true).
