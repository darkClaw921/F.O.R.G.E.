# worktree_remove

Функция tmux-web/src/worktree.rs. Удаляет рабочую копию, НЕ трогая её ветку.

Назначение: 'git worktree remove [--force] <path>' — снимает регистрацию worktree и удаляет его каталог. Ветка, на которую worktree был зачекаучен, СОЗНАТЕЛЬНО НЕ удаляется — это ключевое проектное решение, чтобы не потерять историю коммитов рабочей копии.

Сигнатура: pub async fn worktree_remove(toplevel: &Path, path: &Path, force: bool) -> anyhow::Result<()>.
Реализация: tokio::process::Command 'git worktree remove [--force] <path>' с current_dir(toplevel).

Параметры:
- toplevel: корень репозитория (рабочая директория для git).
- path: путь удаляемой рабочей копии.
- force: если true — добавляется флаг --force. Нужен, когда в worktree есть незакоммиченные изменения или неотслеживаемые файлы, иначе git откажется удалять 'грязную' копию.

Возврат: Ok(()) при успехе; при ненулевом exit git — Err с кодом и обрезанным stderr. Spawn-ошибка через anyhow::Context.

Ограничения/связи: используется в двух местах — (1) delete_worktree_window (force=true, штатное удаление; фронтенд предупреждает о потере незакоммиченных изменений), (2) create_worktree_window как ОТКАТ (force=true), если worktree создан, но tmux-окно открыть не удалось. Ветка forge/… переживает удаление в обоих случаях.
