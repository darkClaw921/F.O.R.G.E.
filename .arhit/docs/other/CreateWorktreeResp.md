# CreateWorktreeResp

DTO ответа POST /api/sessions/:name/windows/worktree (tmux-web/src/main.rs). #[derive(Debug, Serialize)] struct с полями: branch: String ('forge/wt-<ts>' — новая ветка рабочей копии), path: String (абсолютный путь '.../.forge-worktrees/wt-<ts>'), window: String ('wt:<ts>' — имя tmux-окна). Сериализуется в JSON тело ответа 201 из create_worktree_window. Фронтенд использует эти поля для навигации к созданному окну.
