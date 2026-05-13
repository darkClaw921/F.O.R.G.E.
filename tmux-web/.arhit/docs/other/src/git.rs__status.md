# src/git.rs::status

pub async fn status(cwd: &Path) -> Result<GitStatus>. Запускает git status --porcelain=v2 --branch -z в cwd через tokio::process::Command. 

Шаги:
1. is_inside_work_tree(cwd) — git rev-parse --is-inside-work-tree. Если exit != 0 → return Ok(GitStatus{repo:false, branch:None, head:None, upstream:None, ahead:0, behind:0, clean:true, files:vec![]}).
2. Иначе git status --porcelain=v2 --branch -z. На non-zero exit → bail!('git status failed (exit {:?}): {}', code, stderr).
3. parse_status_v2(stdout) — парсер NUL-разделённого вывода.

Парсинг (parse_status_v2):
- Header-строки начинаются с '# ': branch.head/oid/upstream/ab. Значения '(detached)'/'(initial)' → None.
- '1 XY ...' — обычный entry; XY=staging/worktree-коды; path = 9-е поле.
- '2 XY ...' — renamed/copied; XY+10-е поле path; затем отдельный NUL-токен с orig_path.
- 'u XY ...' — unmerged/conflict (kind='conflict').
- '? path' — untracked (x='?', y='?').
- '! path' — ignored.

Поле staged = (x != ' ' && x != '?'). Поле clean = files.is_empty().

XY → kind через classify(): untracked > conflict (UU/AA/DD) > renamed > copied > added > deleted > modified > unknown.

Edge cases: пустой репо (head=(initial) → None), detached HEAD (branch=(detached) → None), пути с пробелами/UTF-8 не ломают парсинг благодаря NUL-разделителю.
