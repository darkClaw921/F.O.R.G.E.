# dockerTerm

state.dockerTerm — TuiTab-инстанция для вкладки Docker (lazydocker TUI) в tmux-web frontend. Создаётся в initTuiTabs() через createTuiTab({name:'lazydocker', wsPath:'/ws/lazydocker', activeTabName:'docker', ...}). Лежит в tmux-web/static/app.js.

## Что это

Структура с теми же полями, что и state.gitTerm:
- term, fit — xterm.js Terminal + FitAddon (заполняются при первом mount).
- ws — WebSocket к /ws/lazydocker?cwd=...&cols=...&rows=... .
- mounted, currentCwd, errorSticky, resizeObserver — служебные флаги.
- name='lazydocker', activeTabName='docker'.
- Методы: mount, connect, close, switchCwd, showBanner, hideBanner, retry, openForActiveProject.

## DOM-привязка

refs.termEl → #docker-term, placeholderEl → #docker-placeholder, errorEl → #docker-error, errorTextEl → #docker-error-text, retryBtn → #docker-error-retry, closeBtn → #docker-error-close, installHelpEl → #docker-install-help, installListEl → #docker-install-list. См. tmux-web/static/index.html (блок #docker, зеркальный #git).

## install-help

При WS-ошибке вида 'lazydocker not found' эвристика createTuiTab показывает install-banner со списком LAZYDOCKER_INSTALL_ENTRIES (Homebrew, Linux script, AUR, Scoop, Go). detectClientOS() сортирует подходящие команды наверх.

## Жизненный цикл

- В bootstrap() initTuiTabs() инициализирует state.dockerTerm.
- При switchTab('docker') — state.dockerTerm.openForActiveProject() → mount + connect /ws/lazydocker.
- При switchTab(другая) — state.dockerTerm.close('tab switched away') (см. app.js ≈1401).
- При смене активного проекта — switchActiveProject вызывает state.dockerTerm.openForActiveProject() (см. ≈1437).
- В beforeunload — state.dockerTerm.close('beforeunload').

## Связи

- /ws/lazydocker (backend) → tmux-web/src/ws.rs::lazydocker_attach → handle_tui_socket<F> с spawn_lazydocker.
- createTuiTab — factory.
- initTuiTabs — инициализатор.
- LAZYDOCKER_INSTALL_ENTRIES — список команд установки.
