# TUI-tabs Framework

Архитектурная страница: generic-механизм для одиночных TUI-вкладок (xterm + WebSocket + PTY-bridge) в tmux-web.

В Phase 1-2 (forge-ddyl / forge-chjx) lazygit-инфраструктура была обобщена: вместо одной функции на один TUI — generic factory на frontend (createTuiTab) и generic handler на backend (handle_tui_socket<F>). Это позволило в одном коммите добавить две новых вкладки (Docker / lazydocker, Find / television) без копирования кода.

## Слои

### Backend (Rust)

1. tmux-web/src/pty.rs — spawn-функции (тонкая обёртка над portable-pty):
   - spawn_lazygit(cwd, cols, rows) → lazygit
   - spawn_lazydocker(cwd, cols, rows) → lazydocker
   - spawn_television(cwd, cols, rows) → tv
   Все возвращают anyhow::Result<PtyHandle>. Идентичная структура: openpty → CommandBuilder с TERM=xterm-256color + cwd → spawn_command (с осмысленной error context, подсказка по установке) → drop(slave) → reader/writer → PtyHandle.

2. tmux-web/src/ws.rs:
   - parse_lazygit_query(raw_query) — общий парсер ?cwd=...&cols=...&rows=... для всех TUI.
   - async fn handle_tui_socket<F>(socket, q, spawn_fn, label) — generic-handler, F: Fn(&Path, u16, u16) -> Result<PtyHandle> + Send + Sync + 'static. Внутри: spawn → spawn_pty_reader → tokio::select! на WS recv / pty EOF. SwitchCwd → kill+spawn через ту же spawn_fn.
   - pub async fn lazygit_attach / lazydocker_attach / telescope_attach — handler-функции для соответствующих маршрутов. Тонкие обёртки: parse_query → ws.on_upgrade → handle_tui_socket(socket, q, spawn_<tui>, label). Поддерживают ?server=<id> для remote-mode (remote_proxy::proxy_websocket с upstream_path).

3. tmux-web/src/main.rs — регистрация маршрутов:
   - .route('/ws/lazygit', get(ws::lazygit_attach))
   - .route('/ws/lazydocker', get(ws::lazydocker_attach))
   - .route('/ws/telescope', get(ws::telescope_attach))

### Frontend (JS)

1. tmux-web/static/index.html — три зеркальных блока: #git, #docker, #telescope. Каждый содержит:
   - tab-кнопку (#tab-git, #tab-docker, #tab-telescope) в top-bar.
   - {prefix}-placeholder — заглушка 'Select a project to open <tui>'.
   - {prefix}-error — banner с .tui-error-text/.tui-error-retry/.tui-error-close.
   - {prefix}-install-help — install-help блок (.tui-install-title + ul.tui-install-list + .tui-install-link).
   - {prefix}-term — xterm-контейнер.

2. tmux-web/static/style.css — общие .tui-* классы: .tui-term, .tui-placeholder, .tui-error, .tui-error-text, .tui-error-retry, .tui-error-close, .tui-install-help, .tui-install-title, .tui-install-list, .tui-install-link, плюс .os-label/.os-cmd/.os-copy для install-списка. Все совместимы с темами через CSS-переменные.

3. tmux-web/static/app.js:
   - createTuiTab({name, wsPath, activeTabName, refs, installHelp}) — factory. Возвращает tabState с полями term/fit/ws/mounted/currentCwd/errorSticky и методами mount/connect/close/switchCwd/showBanner/hideBanner/retry/openForActiveProject.
   - initTuiTabs() — bootstrap: создаёт state.gitTerm / state.dockerTerm / state.telescopeTerm.
   - LAZYGIT_INSTALL_ENTRIES / LAZYDOCKER_INSTALL_ENTRIES / TELESCOPE_INSTALL_ENTRIES — per-OS install commands.
   - detectClientOS() + copyToClipboardSafe() — utility.

## Контракт WebSocket

Идентичен для всех TUI-эндпоинтов (/ws/lazygit, /ws/lazydocker, /ws/telescope):

Query:
  ?cwd=<abs-path>&cols=<u16>&rows=<u16>[&server=<id>]
  cwd обязателен. cols/rows — defaults 80/24. server — только в remote-mode.

Frames:
  - Binary (browser → server): raw bytes (user input) → write в PTY stdin.
  - Binary (server → browser): raw bytes (PTY stdout) → term.write.
  - Text JSON (browser → server):
      {\"type\":\"resize\",\"cols\":<u16>,\"rows\":<u16>}
      {\"type\":\"switch_cwd\",\"cwd\":\"<abs-path>\"}
  - Text JSON (server → browser):
      {\"type\":\"error\",\"message\":\"<text>\"}
      После error — Close frame, WS закрывается. Фронтенд показывает banner.

DTO в Rust:
  LazygitQuery { cwd: String, cols: u16 = 80, rows: u16 = 24 }
  LazygitControl { Resize{cols,rows} | SwitchCwd{cwd} }
  ErrorFrame { type: 'error', message: String }
  Имена сохранены для backward-compat — структуры используются для ВСЕХ TUI.

## install-help структура

Каждый TUI имеет installHelp.entries — список объектов {id, label, cmd}:
  - id — машинно-читаемый ключ для detectClientOS-сортировки ('mac','mac-port','linux-debian','linux-arch','linux-fedora','windows','windows-scoop','go').
  - label — человекочитаемая метка ('macOS (Homebrew)', 'Arch Linux', ...).
  - cmd — команда (одна или несколько строк через \\n).

Алгоритм показа: при WS Text-frame {type:'error',message}, если message.toLowerCase() содержит installHelp.binary AND ('not found' OR 'no such file') — message заменяется на installHelp.notFoundMsg, install-help раскрывается. detectClientOS() сортирует подходящие entries наверх (с маркером .detected). Каждый cmd снабжён кнопкой Copy (Clipboard API → fallback textarea+execCommand).

## Как добавить новую TUI-вкладку (пошагово)

Пример: добавление 'btop' (system monitor) как новой вкладки.

### Backend

1. В pty.rs добавить:
   pub fn spawn_btop(cwd: &Path, cols: u16, rows: u16) -> Result<PtyHandle> {
       // openpty + CommandBuilder::new('btop') + cmd.cwd(cwd) + TERM + spawn + drop(slave) + reader/writer
   }
   В with_context добавить подсказку по установке.

2. В ws.rs добавить:
   pub async fn btop_attach(ws, State(state), Query(raw)) -> Response {
       // 1. extract_server_id + remote proxy если есть (upstream_path='/ws/btop')
       // 2. parse_lazygit_query
       // 3. ws.on_upgrade(|socket| handle_tui_socket(socket, q, spawn_btop, 'btop'))
   }
   В use: добавить spawn_btop в импорт из crate::pty.

3. В main.rs: .route('/ws/btop', get(ws::btop_attach))

### Frontend

4. В index.html:
   - В top-bar: <button id=\"tab-btop\" class=\"tab-btn\" type=\"button\">Btop</button>
   - В <main>: панель #btop (зеркальная #docker) со всеми {prefix}-* элементами и .tui-* классами.

5. В style.css: добавить #btop в селекторы рядом с #docker/#telescope (display:flex / [hidden] правила).

6. В app.js:
   - DOM-refs в начале файла:  = document.getElementById('tab-btop');  = document.getElementById('btop-term'); и остальные.
   - state.btopTerm = null (в state-объекте).
   - BTOP_INSTALL_ENTRIES = [{...}, ...].
   - В initTuiTabs() добавить:
       state.btopTerm = createTuiTab({
         name: 'btop',
         wsPath: '/ws/btop',
         activeTabName: 'btop',
         refs: {termEl: , placeholderEl: , errorEl: , ...},
         installHelp: {binary:'btop', notFoundMsg:'...', entries: BTOP_INSTALL_ENTRIES},
       });
   - В switchTab(): handle prev==='btop' (close) + activeTab==='btop' (open).
   - В switchActiveProject(): state.btopTerm.openForActiveProject() при tabName==='btop'.
   - В beforeunload-handler: state.btopTerm.close('beforeunload').

### Готово

Никакой логики WS/PTY/error/install-help/reconnect/resize/switchCwd писать не нужно — она вся в handle_tui_socket и createTuiTab.

## Ограничения и будущие улучшения

- LazygitQuery / LazygitControl сохраняют исторические имена. Если когда-нибудь окажется, что какому-то TUI нужен дополнительный control-вариант (например, hot-reload config) — нужно либо расширить общий enum, либо завести параллельный.
- handle_tui_socket принимает только cwd-ориентированные TUI. Если потребуется session-ориентированный (как tmux attach) — нужен другой generic-handler или extension trait.
- install-help binary-detection — простая substring-эвристика. Если backend изменит wording ошибки — фронтенд не покажет install-help. Можно перейти на типизированный error-code (например, ErrorFrame.kind='binary_not_found').
- ResizeObserver/window 'resize' — fit() вызывается только когда state.activeTab совпадает с tab.activeTabName. Корректно для текущего UX (одна вкладка активна), но если когда-нибудь будет split-view — потребуется правка.

## Ссылки на код

- Backend: tmux-web/src/pty.rs (spawn_*), tmux-web/src/ws.rs (handle_tui_socket, *_attach), tmux-web/src/main.rs (routes).
- Frontend: tmux-web/static/app.js (createTuiTab, initTuiTabs), tmux-web/static/index.html (#git, #docker, #telescope), tmux-web/static/style.css (.tui-*).
- Документация элементов:
  - createTuiTab, initTuiTabs, dockerTerm, telescopeTerm (frontend)
  - handle_tui_socket, lazydocker_attach, telescope_attach, spawn_lazydocker, spawn_television (backend)