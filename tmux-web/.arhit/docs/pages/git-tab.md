# Git tab — общая сводка фичи

Полнофункциональная вкладка Git для tmux-web: live-снапшот рабочего дерева активного проекта + commit graph. Всё работает поверх git CLI (subprocess через tokio::process::Command), без libgit2.

## Backend (Rust, axum)

### src/git.rs — core git-модуль

Модуль с публичными async-функциями + структурами для сериализации через serde.

Структуры (все pub, derive Serialize):
- GitStatus { repo, branch, head, upstream, ahead, behind, clean, files: Vec<GitFile> }
- GitFile { path, orig_path, x: char, y: char, staged: bool, kind: &'static str }
- GitCommit { hash, abbrev, parents: Vec<String>, author, email, date, subject, refs: Vec<String> }

Функции:
- status(cwd) — git status --porcelain=v2 --branch -z; не репо → GitStatus{repo:false}.
- log(cwd, limit) — git log с custom форматом %H\\x1f%h\\x1f%P… разделителями \\x1f/\\x1e. Пустой репо → Ok(vec![]).
- stage(cwd, paths) — git add -- paths.
- unstage(cwd, paths) — git restore --staged --; fallback git rm --cached для empty-репо без HEAD.
- commit(cwd, msg) — git -c commit.gpgsign=false commit -m. Возвращает abbrev hash из stdout.

### src/main.rs — handlers (5 endpoints)

- GET  /api/git/status → Json<GitStatus>. cwd = state.projects.read().active().path.
- GET  /api/git/log?limit=N → Json<Vec<GitCommit>>. limit clamped до 500, default 100.
- POST /api/git/stage    body {paths:[...]}; пустой → 400 'paths is empty'.
- POST /api/git/unstage  тот же контракт.
- POST /api/git/commit   body {message:'...'}; пустой/whitespace → 400 'empty message'. На успех → {hash:'<full sha>'}.

Ошибки git → 400 со stderr (или 500 для status: реальные git-сбои).

## Frontend (static/index.html + style.css + app.js)

### index.html — Phase 3

- #tab-bar > <button id='tab-git'> + <span id='git-status-meta'>.
- <div id='git' hidden> — pane с тремя секциями (CSS grid 1fr 1fr 1.4fr):
  - #git-files-pane: #git-staged-list и #git-unstaged-list (чекбоксы + два .git-badge X/Y + path).
  - #git-commit-pane: textarea #git-commit-msg + button #git-commit-btn (.primary, disabled до stage) + p#git-commit-error.
  - #git-graph-pane: <canvas id='git-graph-canvas'>.

### style.css — Phase 3

Стили в конце файла (~2.9KB). Используют все темы через CSS-переменные (--bg, --bg-toolbar, --accent, --warn, --danger, --success). .git-badge цвета:
- modified=warn, added=success, deleted=danger, untracked=fg-dim, renamed=accent, conflict=danger.

### app.js — Phase 4 (status + commit logic)

- state.gitStatus, state.gitLog, state.gitPollTimer.
- GIT_POLL_INTERVAL_MS = 5000.
- switchTab() расширен: на 'git' — fetchGitAll() + startGitPolling(); при уходе — stopGitPolling().
- fetchGitAll → Promise.all([fetchGitStatus, fetchGitLog]).
- fetchGitStatus / fetchGitLog: не throw, console.warn на сбой, state не меняется (нет UI flicker).
- renderGitToolbar: branch/ahead-behind/upstream; detached HEAD → '(detached) <abbrev>'.
- renderGitFiles: разделение на staged/unstaged; conflict-файлы — disabled checkbox.
- buildGitFileRow: <label> с checkbox, двумя X/Y badge, path (textContent — XSS-safe).
- toggleStage(paths, action): POST /api/git/{stage|unstage}; всегда вызывает fetchGitStatus после, даже на ошибке.
- commitNow / updateCommitBtnState: enable Commit только при наличии staged-файлов И непустого message; пустой → 400 от backend → отображается в #git-commit-error.

### app.js — Phase 5 (commit graph)

- computeGitLanes(commits): два прохода. Первый — назначает каждому коммиту .lane (индекс колонки) и .row (==i); второй — рассчитывает edges {fromLane, fromRow, toLane, toRow}. Для родителя вне видимости → toRow=commits.length, toLane=fromLane (рисуется 'обрыв вниз').
- renderGitGraph: canvas resize по DPR (devicePixelRatio); рисует edges (curved bezier) → потом nodes (filled circles цветом lane) → потом метаданные справа (abbrev, refs, subject).
- Палитра 7 цветов (GIT_LANE_PALETTE), GIT_LANE_W=18, GIT_ROW_H=24, GIT_NODE_R=5, GIT_META_W=600.

## Edge cases

- Не git-репо: backend → {repo:false}, frontend → toolbar 'Не git-репозиторий', file-lists и graph пусты.
- Пустой репо без коммитов: status работает (head=None), log → []. Unstage fallback на git rm --cached.
- Detached HEAD: branch=None, head=full SHA → toolbar показывает '(detached) abbrev'.
- Renamed файлы: GitFile.orig_path заполнен → 'orig → new' в UI.
- Conflict (UU/AA/DD/AU/UA/DU/UD): kind='conflict', checkbox disabled с tooltip.
- gpg-signing: -c commit.gpgsign=false при commit чтобы не зависнуть на passphrase prompt в headless.
- Polling: 5s setInterval только пока активен Git tab. switchTab прочь → clearInterval. Идемпотентно — повторный switch не создаёт двойных таймеров.
- Network failure / HTTP 5xx во время polling: state не трогается, старый снапшот остаётся виден; console.warn для DevTools.

## Phase 6 verification (2026-05-10)

cargo build clean (только pre-existing pty.rs dead_code). API smoke-tests прошли:
- /healthz → ok
- GET /api/git/status → repo:true c branch/head/files (форма JSON соответствует контракту)
- GET /api/git/log?limit=10 → массив с hash/abbrev/parents/subject/refs
- POST commit с empty/whitespace message → 400 'empty message'
- POST stage/unstage с paths:[] → 400 'paths is empty'
- limit clamping работает (1000 не ломает)

Реальные mutating-операции (commit, stage/unstage с непустыми paths) НЕ выполнялись от агента (правило проекта: не запускать git add/commit). Ручная проверка пользователем через UI.