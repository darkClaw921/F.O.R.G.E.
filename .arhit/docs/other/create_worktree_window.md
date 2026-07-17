# create_worktree_window

Хендлер POST /api/sessions/:name/windows/worktree (tmux-web/src/main.rs). Реализует бэкенд «радужной кнопки» — создание нового tmux-окна в изолированной git-worktree.

Сигнатура: async fn create_worktree_window(State<AppState>, AxumPath<String> name, Query<HashMap<String,String>>, body: Bytes) -> Result<Response, (StatusCode, String)>. Тело не обязательно (авто-режим).

Флоу:
1. try_proxy_to_remote(POST, /api/sessions/{name}/windows/worktree) — проброс на удалённый узел при ?server=.
2. tmux::session_cwd(name) — cwd активной панели сессии (Err→400).
3. worktree::repo_toplevel(cwd) — настоящий git-toplevel; Ok(None)→400 'сессия не в git-репозитории', Err→400.
4. worktree::ensure_gitignore_entry(toplevel) — прячет .forge-worktrees/ (некритично).
5. worktree::alloc_worktree_name(worktrees_base(toplevel)) → (dir_name 'wt-<ts>', wt_path); branch='forge/'+dir_name.
6. worktree::worktree_add(toplevel, wt_path, branch) — worktree на новой ветке; Err→400.
7. win_name = dir_name.replacen('wt-','wt:',1); tmux::new_window_in(name, win_name, wt_path). При Err — ОТКАТ: worktree_remove(force) + 400.
8. Успех: 201 + Json(CreateWorktreeResp{branch,path,window}).

Зависимости: crate::tmux (session_cwd, new_window_in), crate::worktree (repo_toplevel, ensure_gitignore_entry, alloc_worktree_name, worktrees_base, worktree_add, worktree_remove), try_proxy_to_remote. Ответный DTO — CreateWorktreeResp.
