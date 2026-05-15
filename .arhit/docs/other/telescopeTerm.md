# telescopeTerm

state.telescopeTerm — TuiTab-инстанция для вкладки Telescope (tv / television fuzzy finder) в tmux-web frontend. Создаётся в initTuiTabs() через createTuiTab({name:'telescope', wsPath:'/ws/telescope', activeTabName:'telescope', ...}). Лежит в tmux-web/static/app.js.

## Что это

Структура с теми же полями, что и state.gitTerm/dockerTerm:
- term, fit — xterm.js Terminal + FitAddon.
- ws — WebSocket к /ws/telescope?cwd=...&cols=...&rows=... .
- mounted, currentCwd, errorSticky, resizeObserver — служебные флаги.
- name='telescope', activeTabName='telescope'.
- Методы: mount, connect, close, switchCwd, showBanner, hideBanner, retry, openForActiveProject.

## DOM-привязка

refs.termEl → #telescope-term, placeholderEl → #telescope-placeholder, errorEl → #telescope-error, errorTextEl → #telescope-error-text, retryBtn → #telescope-error-retry, closeBtn → #telescope-error-close, installHelpEl → #telescope-install-help, installListEl → #telescope-install-list. См. tmux-web/static/index.html (блок #telescope, зеркальный #git).

## install-help

При WS-ошибке вида 'television (tv) not found' эвристика createTuiTab (binary='tv' + 'not found') показывает install-banner со списком TELESCOPE_INSTALL_ENTRIES (Homebrew, pacman, dnf+copr, cargo install --locked television).

## Жизненный цикл

- В bootstrap() initTuiTabs() инициализирует state.telescopeTerm.
- При switchTab('telescope') — state.telescopeTerm.openForActiveProject() → mount + connect /ws/telescope.
- При switchTab(другая) — state.telescopeTerm.close('tab switched away') (см. app.js ≈1404).
- При смене активного проекта — switchActiveProject вызывает state.telescopeTerm.openForActiveProject() (см. ≈1439).
- В beforeunload — state.telescopeTerm.close('beforeunload').

## Особенность tv

Бинарь television устанавливается как 'tv' (короткое имя). cwd передаётся как корень fuzzy-поиска — tv использует его для file-source provider'а по умолчанию.

## Связи

- /ws/telescope (backend) → tmux-web/src/ws.rs::telescope_attach → handle_tui_socket<F> с spawn_television.
- createTuiTab — factory.
- initTuiTabs — инициализатор.
- TELESCOPE_INSTALL_ENTRIES — список команд установки.
