# tmux-web/static/index.html

HTML-шаблон tmux-web frontend. Содержит layout: левый sidebar (sessions/projects), top-bar с tab-кнопками, и панели-вкладки terminal/tasks/git/docker/telescope.

## Tab-кнопки (top-bar)

- #tab-terminal — Terminal (tmux attach).
- #tab-tasks — Tasks (beads-board).
- #tab-git — Git (lazygit TUI).
- #tab-docker — Docker (lazydocker TUI, Phase 2).
- #tab-telescope — Find (television fuzzy finder, Phase 2). Подпись 'Find' — semantically более понятна, чем 'Telescope'.

## Панели вкладок

Phase 4: git-pane — xterm.js терминал к /ws/lazygit.
Phase 2 (forge-chjx): добавлены панели #docker и #telescope — зеркальные #git, но с другими ID-суффиксами.

### Общая структура TUI-панелей (#git, #docker, #telescope)
- {prefix}-placeholder — текст-заглушка ('Select a project to open <tui>'), виден когда нет активного проекта. Phase 2 заменён на класс .tui-placeholder.
- {prefix}-error — banner-плашка с .tui-error-text, .tui-error-retry и .tui-error-close (×). hidden по умолчанию.
- {prefix}-install-help — раскрывающийся блок с install-командами (виден при binary-not-found). Содержит .tui-install-title + ul.tui-install-list + a.tui-install-link.
- {prefix}-term — контейнер xterm.js Terminal (.tui-term). Изолированная инстанция на каждую вкладку.

### DOM ids

git-pane: #git-placeholder, #git-error, #git-error-text, #git-error-retry, #git-error-close, #git-install-help, #git-install-list, #git-term, #git-legacy.

docker-pane (Phase 2): #docker-placeholder, #docker-error, #docker-error-text, #docker-error-retry, #docker-error-close, #docker-install-help, #docker-install-list, #docker-term.

telescope-pane (Phase 2): #telescope-placeholder, #telescope-error, #telescope-error-text, #telescope-error-retry, #telescope-error-close, #telescope-install-help, #telescope-install-list, #telescope-term.

### #git-legacy

Обёртка над старой git-разметкой (toolbar/files/commit/graph), оставлена hidden до Phase 5 cleanup. Нужна чтобы legacy DOM-refs (, ,  и т.д.) не были null при загрузке app.js.

## Подключаемые скрипты

xterm.js + addon-fit + addon-web-links (CDN/embedded). app.js — главный IIFE-modul.

## Связи

- Tab-кнопки → switchTab() в app.js.
- {prefix}-term → state.gitTerm/dockerTerm/telescopeTerm через initTuiTabs() (создаёт createTuiTab инстансы с DOM-refs).
- {prefix}-install-help → install-help блок, показывается createTuiTab при binary-not-found.
