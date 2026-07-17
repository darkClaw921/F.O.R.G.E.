# delete_worktree_window

Хендлер DELETE /api/sessions/:name/windows/:index/worktree (tmux-web/src/main.rs). Удаляет worktree-окно вместе с рабочей копией; ветка forge/… сохраняется (коммиты не теряются).

Сигнатура: async fn delete_worktree_window(State<AppState>, AxumPath<(String,u32)> (name,index), Query<HashMap<String,String>>) -> Result<Response, (StatusCode, String)>.

Флоу:
1. try_proxy_to_remote(DELETE, /api/sessions/{name}/windows/{index}/worktree).
2. tmux::window_cwd(name,index) — cwd окна (Err→400).
3. worktree::repo_toplevel(cwd) — Ok(None)/Err→400.
4. КРИТИЧЕСКАЯ ЗАЩИТА: std::fs::canonicalize(cwd) и canonicalize(worktrees_base(toplevel)); если cwd_canon НЕ starts_with base_canon → 400 'окно не является worktree-окном .forge-worktrees'. Ошибка canonicalize → 400. Предотвращает удаление произвольного/основного каталога.
5. worktree::worktree_remove(toplevel, cwd_canon, force=true) — незакоммиченные изменения теряются (фронтенд предупреждает), ветка НЕ трогается; Err→400 (окно НЕ убиваем).
6. tmux::kill_window(name,index) — worktree уже удалён, при Err только warn + 500.
7. Успех: 204 NO_CONTENT.

Зависимости: crate::tmux (window_cwd, kill_window), crate::worktree (repo_toplevel, worktrees_base, worktree_remove), std::fs::canonicalize, try_proxy_to_remote.
