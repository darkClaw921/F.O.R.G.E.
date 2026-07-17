Фича «Новое окно в git worktree» (радужная кнопка рядом с «+» в шапке tmux-сессии). Даёт изолированную рабочую копию репозитория на новой ветке, чтобы агент работал в своём пространстве, не мешая основному дереву.

== Backend: модуль tmux-web/src/worktree.rs (Фаза 1) ==
Мутирующие git-worktree операции через tokio::process::Command; git.rs остаётся read-only. Публичные функции:
- repo_toplevel(cwd) -> Result<Option<PathBuf>>: git rev-parse --path-format=absolute --git-common-dir с current_dir(cwd); возвращает РОДИТЕЛЯ общего git-каталога (<main>/.git → <main>). Общий git-каталог един для главного дерева и всех linked-worktree, поэтому даже из .forge-worktrees/wt-… возвращается ГЛАВНЫЙ корень репозитория. Важно: наивный git rev-parse --show-toplevel из linked-worktree вернул бы корень самого worktree — это ломало бы и защиту от удаления произвольного каталога, и запуск git worktree remove/add. Не-git каталог / spawn-ошибка / пустой вывод -> Ok(None).
- worktree_add(toplevel, path, branch): git worktree add <path> -b <branch> с current_dir(toplevel). Ненулевой exit -> Err(stderr).
- worktree_remove(toplevel, path, force): git worktree remove [--force] <path>. Ветку НЕ удаляет (история рабочей копии/коммиты агента сохраняются).
- ensure_gitignore_entry(toplevel): идемпотентно дописывает '.forge-worktrees/' в <toplevel>/.gitignore (сравнение по trim построчно; ведущий \n если файл не заканчивается на newline). Ошибки I/O только логируются через tracing::warn! (некритично).
- alloc_worktree_name(base) -> (String, PathBuf): имя wt-<unix_secs>, при коллизии каталога суффикс -2/-3/...; возвращает (имя, base.join(имя)).
- worktrees_base(toplevel) -> PathBuf: <toplevel>/.forge-worktrees.
Модель размещения (решение пользователя — nested): рабочие копии живут в <repo>/.forge-worktrees/<имя>/, каждая на своей новой ветке forge/wt-<ts>. Папка .forge-worktrees/ добавляется в .gitignore, поэтому её содержимое не мусорит в git status.

== Backend: tmux-хелперы (tmux-web/src/tmux.rs, Фаза 1) ==
- new_window_in(session, name: Option<&str>, cwd): как new_window, но -c <cwd> (реальный путь) вместо #{pane_current_path}. Таргет '<session>:' (session-target).
- session_cwd(session) -> Result<String>: tmux display-message -p -t '<session>:' -F '#{pane_current_path}' -> trimmed путь активной панели.
- window_cwd(session, index) -> Result<String>: то же для таргета '<session>:<index>'.
- display_pane_current_path(target): приватный общий хелпер (spawn + проверка exit + trim; пустой результат -> Err).
Двоеточие в таргете обязательно (регресс с числовыми именами сессий). Регистрация: 'mod worktree;' добавлен в main.rs рядом с 'mod git;'.

== Backend: эндпоинты (tmux-web/src/main.rs, Фаза 2) ==
- POST /api/sessions/:name/windows/worktree -> create_worktree_window: proxy_to_remote → session_cwd → repo_toplevel (None → 400 «сессия не в git-репозитории») → ensure_gitignore_entry → alloc_worktree_name → ветка forge/wt-<ts> → worktree_add → окно wt:<ts> через new_window_in. При падении создания окна — откат worktree_remove(force). Успех → 201 Json CreateWorktreeResp{branch,path,window}.
- DELETE /api/sessions/:name/windows/:index/worktree -> delete_worktree_window: proxy → window_cwd → repo_toplevel → ЗАЩИТА: canonicalize(cwd).starts_with(canonicalize(worktrees_base(toplevel))), иначе 400 (не удаляем произвольный каталог) → worktree_remove(force=true) → kill_window → 204. Ветка сохраняется.
Роуты добавлены рядом с блоком windows; статический сегмент 'worktree' и параметр ':index' — рабочие сиблинги в matchit 0.7.3 (как /api/sessions/history vs /api/sessions/:name), конфликта нет.

== Frontend (tmux-web/static, Фаза 3) ==
- index.html: кнопка #window-new-worktree (class .window-worktree-btn, глиф ⑂) после #window-new внутри #window-bar.
- js/core/dom.js: экспорт $windowNewWorktreeBtn.
- js/sessions/windows.js: createWorktreeWindow() — авто-режим (без prompt), POST .../windows/worktree, alert при ошибке, fetchWindows(). killWindow(index,name) переписан: окна с именем-префиксом 'wt:' закрываются через DELETE .../windows/:index/worktree с усиленным confirm (несохранённые изменения worktree теряются, коммиты в ветке forge/... сохраняются); обычные окна — прежний DELETE .../windows/:index.
- js/core/bootstrap.js: импорт + click-привязка createWorktreeWindow к $windowNewWorktreeBtn.
- css/window-bar.css: .window-worktree-btn — анимированный радужный linear-gradient (background-size 300%, @keyframes wt-rainbow 4s linear infinite), :hover brightness(1.15), @media (prefers-reduced-motion: reduce) → animation:none.
Статика встроена rust-embed → UI-правки требуют пересборки devforge.

== Проверка (Фаза 4) ==
cargo build -p devforge — чисто, 0 warnings. cargo test worktree:: — 7/7 (включая regress repo_toplevel_from_linked_worktree_returns_main_root). HTTP e2e (devforge на порту 7399, legacy localhost без auth, временный git-репо + tmux-сессии): 19/19 PASS — create→201 (worktree, ветка, gitignore, окно wt: с cwd=worktree), delete→204 (каталог удалён, ветка сохранена, окно убито), негатив не-git→400, регресс обычных окон. Единственный след в git status после первого использования — одноразовая правка .gitignore (by design).