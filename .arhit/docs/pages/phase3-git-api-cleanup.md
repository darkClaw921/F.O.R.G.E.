# Phase 3 forge-gda: Cleanup устаревшего REST git API

## Контекст
До этой фазы tmux-web имел REST API для git-операций (status, log, stage, unstage, commit), реализованный в tmux-web/src/git.rs и зарегистрированный handler-функциями в tmux-web/src/main.rs. После реализации /ws/lazygit (Phase 2 forge-nbl) — lazygit TUI в браузере через xterm.js — REST API стал избыточным: lazygit покрывает все его use-case'ы и даёт UX лучше.

## Что удалено

### tmux-web/src/main.rs
- Routes:
  - .route('/api/git/status', get(get_git_status))
  - .route('/api/git/log', get(get_git_log))
  - .route('/api/git/stage', post(post_git_stage))
  - .route('/api/git/unstage', post(post_git_unstage))
  - .route('/api/git/commit', post(post_git_commit))
- Handler-функции:
  - async fn get_git_status(State<AppState>) -> Result<Json<git::GitStatus>, ...>
  - async fn get_git_log(State<AppState>, Query<LogQuery>) -> Result<Json<Vec<git::GitCommit>>, ...>
  - async fn post_git_stage(State<AppState>, Json<PathsReq>) -> Result<StatusCode, ...>
  - async fn post_git_unstage(State<AppState>, Json<PathsReq>) -> Result<StatusCode, ...>
  - async fn post_git_commit(State<AppState>, Json<CommitReq>) -> Result<Json<Value>, ...>
- DTO структуры:
  - struct LogQuery { limit: Option<u32> }
  - struct PathsReq { paths: Vec<String> }
  - struct CommitReq { message: String }
- mod git;

### tmux-web/src/git.rs
Файл удалён физически (~580 строк): GitStatus/GitFile/GitCommit структуры, status()/log()/stage()/unstage()/commit() async-функции, parse_entry/parse_entry_unmerged/classify хелперы для git status --porcelain v2.

## Что осталось / куда переехал git-функционал

- /ws/lazygit (tmux-web/src/ws.rs::lazygit_attach) — единственная git-точка backend'а. Принимает Query{cwd,cols,rows}, спавнит lazygit в PTY (pty.rs::spawn_lazygit), проксирует stdin/stdout через WebSocket.
- LazygitControl::SwitchCwd — позволяет на лету переключать lazygit на другой проект без переоткрытия WS.

## Acceptance
- cargo check tmux-web — clean (0 errors).
- grep crate::git tmux-web/src/ — пусто.
- ls tmux-web/src/ — git.rs отсутствует.
- ls tmux-web/src/main.rs — нет упоминаний /api/git/.

## Связанные задачи
- forge-gda.1: Удалить REST git routes (closed)
- forge-gda.2: Удалить git handler-функции и DTO (closed)
- forge-gda.3: Удалить mod git и файл git.rs (closed)
- forge-gda.4: Cargo check после cleanup (closed)
- forge-gda.5: Обновить arhit doc (closed)
