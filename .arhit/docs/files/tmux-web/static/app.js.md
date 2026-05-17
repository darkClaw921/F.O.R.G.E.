# tmux-web/static/app.js

Frontend tmux-web (xterm + WS-attach + tasks/git/docker/telescope/projects/themes/remote).

## Сборка
Один большой IIFE-module (~6000 строк) в tmux-web/static/app.js. Embedded в бинарь через rust-embed.

## Ключевые блоки

### state (объект)
Центральное хранилище в памяти. Поля:
- ws, currentSession, sessions, term, fitAddon — основной /ws/attach + xterm.
- attachWsBackoffStep / attachWsReconnectTimer / attachWsClosedByUs / attachWsOrigin (Phase 7) — backoff-reconnect /ws/attach.
- activeTab ('terminal'|'tasks'|'git'|'docker'|'telescope'), tasksData, tasksWs (+backoff/timer/closedByUs), todosData/todosWs.
- projects, activeProjectId, projectFilter — мульти-проекты.
- remoteMode, serverVersion, healthzLoaded — /healthz bootstrap.
- remoteServers (Array<RemoteServerView>), remoteOnline (Map<id, 'online'|'offline'|'unknown'>).
- remoteSessions/remoteProjects (Map<server_id, ...>) — aggregated view.
- activeOrigin ('all'|'local'|server_id) — origin-фильтр.
- gitTerm, dockerTerm, telescopeTerm — TuiTab инстансы (см. createTuiTab/initTuiTabs ниже).
- activeTheme — Phase 3 themes.

### TUI-tabs framework (Phase 2, forge-chjx)

Generic factory createTuiTab({name, wsPath, activeTabName, refs, installHelp}) собирает изолированную xterm-инстанцию, говорящую по WebSocket с PTY на бэкенде. Контракт WS идентичен для всех TUI:
- query: ?cwd=<path>&cols=<n>&rows=<n>[&server=<id>]
- Binary frame в обе стороны = pty bytes.
- Text frame от клиента: {type:'resize',cols,rows} / {type:'switch_cwd',cwd}.
- Text frame от сервера: {type:'error',message:...} — показывается в .tui-error banner с install-help при binary-not-found.

initTuiTabs() в bootstrap создаёт три TuiTab. INSTALL_ENTRIES константы (LAZYGIT_INSTALL_ENTRIES/LAZYDOCKER_INSTALL_ENTRIES/TELESCOPE_INSTALL_ENTRIES) содержат per-OS команды установки.

### Hotkeys

- **Cmd+B (mac) / Ctrl+B (other)**: toggle sidebar. Window-capture-phase listener (app.js:605).
  - **Fix forge-93l9**: на не-mac когда фокус в xterm (.xterm-helper-textarea / inside .xterm) хоткей пропускается, чтобы tmux prefix Ctrl+B корректно уходил в PTY.
- **Cmd+C / Ctrl+Shift+C** (xterm.attachCustomKeyEventHandler в app.js:1646): copy выделения в clipboard. Остальные Ctrl-комбинации (Ctrl+C SIGINT, Ctrl+D EOF и т.п.) идут напрямую в PTY через xterm onData.

### Ключевые функции
- bootstrap() — loadHealthz, тема, initTerminal, sidebar, project bar, WS connect, initTuiTabs().
- connectWs(sessionName, origin) — /ws/attach с ?server=<origin>. Backoff reconnect.
- disconnectWs() — помечает attachWsClosedByUs.
- scheduleAttachWsReconnect() — backoff [2s,4s,8s,16s,32s,60s] + jitter.
- connectTasksWs / connectTodosWs — WS подписки.
- switchTab(tabName) — переключение вкладок.
- switchActiveProject(projectId) — смена проекта.
- probeRemoteServer(serverId), renderSidebar / renderOriginSection, isRemoteMode(), fetchRemoteServers, openSettingsModal('remotes').

## reconnect & health probe

### Health probe per-server
remoteProbeState: Map<server_id, {timer, step, inFlight}>.
REMOTE_PROBE_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000]. STEADY_INDEX=1 (4s online).

### WS reconnect
- /ws/attach: ATTACH_WS_BACKOFFS_MS = [2s, 4s, 8s, 16s, 32s, 60s] + jitter.
- /ws/tasks, /ws/todos: [1s, 2s, 5s, 10s] + polling fallback.
- /ws/lazygit, /ws/lazydocker, /ws/telescope: manual retry через UI banner.

## Mobile-расширения (Phase A/B/C)

### isMobileViewport() и matchMedia

В IIFE-scope создан `_mqlMobile = window.matchMedia('(max-width: 768px)')`. Функция-хелпер `isMobileViewport()` возвращает `_mqlMobile.matches`. На событие `change` _mqlMobile подписан listener, который:
- Перерисовывает sidebar (вызывает `applySidebarCollapsed()` / `restoreSidebarState()`).
- Применяет `applyTerminalFontSize()`.
- Прячет/показывает `.tui-quick-bar` через JS (на desktop — hidden=true).

Это позволяет реактивно переключать UI между mobile/desktop без перезагрузки страницы (например, при повороте экрана или resize окна DevTools).

### applySidebarCollapsed / toggleSidebar / restoreSidebarState (mobile-ветка)

На desktop поведение прежнее (sidebar collapsible через класс `.collapsed` на body).
На mobile (через `isMobileViewport()`):
- `toggleSidebar()` → вызывает `setMobileSidebarOpen(!body.classList.contains('sidebar-open'))`.
- `restoreSidebarState()` на mobile всегда стартует с закрытым sidebar (не использует localStorage).

### setMobileSidebarOpen(open: boolean)

Управляет классом `body.sidebar-open` и видимостью `#sidebar-overlay`:
- `open=true`: `body.classList.add('sidebar-open')`, `$sidebarOverlay.hidden = false`.
- `open=false`: `body.classList.remove('sidebar-open')`, `$sidebarOverlay.hidden = true`.

### #sidebar-overlay handlers

- Click по overlay → `setMobileSidebarOpen(false)`.
- Esc keydown (на body) — закрывает sidebar если открыт.

### applyTerminalFontSize()

Применяет шрифт ко всем активным Terminal-инстансам (`state.term`, `state.gitTerm.term`, `state.dockerTerm.term`, `state.telescopeTerm.term`). Размер:
- `TERM_FONT_SIZE_DESKTOP = 13` (px).
- `TERM_FONT_SIZE_MOBILE = 11` (px).

После изменения `term.options.fontSize` вызывается `fitAddon.fit()` чтобы пересчитать cols/rows.

Вызывается из bootstrap, при подключении нового TUI-tab, и при matchMedia change.

### window.QuickCmd.onPtyInput хуки

Хуки добавлены в местах `term.onData(...)`:
- Main xterm в bootstrap/initTerminal (~app.js:215-216):
  ```js
  term.onData(data => {
    if (window.QuickCmd) window.QuickCmd.onPtyInput(data);
    // ... отправка в WS
  });
  ```
- В `createTuiTab` factory (~app.js:1827-1828) — аналогичный хук на каждый TUI-xterm.

Это значит, что quick-cmd.js видит ввод пользователя со всех PTY (main + git/docker/telescope) и автоматически трекает частоту команд.

### sendToActivePty(text: string)

Public-функция (в конце IIFE), маршрутизирующая байты в активный PTY по `state.activeTab`:
- `terminal` → `state.ws.send(text)` (если ws open).
- `git` → `state.gitTerm?.ws?.send(text)`.
- `docker` → `state.dockerTerm?.ws?.send(text)`.
- `telescope` → `state.telescopeTerm?.ws?.send(text)`.
- `tasks` или нет активной WS → no-op.

WebSocket принимает text как binary через TextEncoder перед .send (так же, как обычные term.onData отправки).

### window.ForgeApp

В самом конце IIFE экспорт: `window.ForgeApp = { sendToActivePty, state }`. Это контракт интеграции для внешних модулей (в первую очередь — `quick-cmd.js`). `state` экспортируется по reference чтобы quick-cmd.js мог читать `activeTab`.

## Зависимости

- xterm.js + addon-fit + addon-web-links — рендеринг терминала.
- `quick-cmd.js` (Phase B/C) — потребитель `window.ForgeApp.sendToActivePty` и хуков `term.onData`. Подключается отдельно из `index.html`, после `app.js`.

## Связанные файлы

- [tmux-web/static/index.html](tmux-web/static/index.html) — DOM-структура (#sidebar-overlay, #quick-cmd-bar, #git/docker/telescope-quick-bar).
- [tmux-web/static/style.css](tmux-web/static/style.css) — mobile @media-секции (off-canvas, scroll-snap, full-screen modals).
- [tmux-web/static/quick-cmd.js](tmux-web/static/quick-cmd.js) — модуль quick-command bar (потребитель ForgeApp.sendToActivePty).
