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
- gitTerm, dockerTerm, telescopeTerm — TuiTab инстансы (см. createTuiTab/initTuiTabs ниже). Каждая содержит { term, fit, ws, mounted, currentCwd, errorSticky, resizeObserver, name, activeTabName } + методы mount/connect/close/switchCwd/showBanner/hideBanner/retry/openForActiveProject.
- activeTheme — Phase 3 themes.

### TUI-tabs framework (Phase 2, forge-chjx)

Generic factory createTuiTab({name, wsPath, activeTabName, refs, installHelp}) собирает изолированную xterm-инстанцию, говорящую по WebSocket с PTY на бэкенде. Контракт WS идентичен для всех TUI:
- query: ?cwd=<path>&cols=<n>&rows=<n>[&server=<id>]
- Binary frame в обе стороны = pty bytes.
- Text frame от клиента: {type:'resize',cols,rows} / {type:'switch_cwd',cwd}.
- Text frame от сервера: {type:'error',message:...} — показывается в .tui-error banner с install-help при binary-not-found.

initTuiTabs() в bootstrap создаёт три TuiTab и пишет в state.gitTerm/dockerTerm/telescopeTerm. INSTALL_ENTRIES константы (LAZYGIT_INSTALL_ENTRIES/LAZYDOCKER_INSTALL_ENTRIES/TELESCOPE_INSTALL_ENTRIES) содержат per-OS команды установки. detectClientOS + copyToClipboardSafe — utility.

Старые функции mountGitTerm/connectGitWs/closeGitWs/gitSwitchCwd/showGitBanner/hideGitBanner/retryGitConnection/openLazygitForActiveProject оставлены как тонкие алиасы на state.gitTerm.* для обратной совместимости.

### Ключевые функции
- bootstrap() — loadHealthz, тема, initTerminal, sidebar, project bar, WS connect, **initTuiTabs()**.
- connectWs(sessionName, origin) — /ws/attach с ?server=<origin>. Backoff reconnect.
- disconnectWs() — помечает attachWsClosedByUs, чтобы onclose не реконнектился.
- scheduleAttachWsReconnect() — backoff серии ATTACH_WS_BACKOFFS_MS=[2s,4s,8s,16s,32s,60s] + jitter.
- connectTasksWs / connectTodosWs — WS подписки.
- switchTab(tabName) — переключение вкладок. При уходе с docker/telescope/git вызывает state.<term>.close('tab switched away'). При активации вызывает openForActiveProject() соответствующего TuiTab (если ws закрыт).
- switchActiveProject(projectId) — смена проекта. Для git/docker/telescope вызывает openForActiveProject() на каждом TuiTab.
- probeRemoteServer(serverId) — per-server health probe.
- renderSidebar / renderOriginSection — origin-табы и группировка сессий.
- isRemoteMode() — guard для UI-features remote-режима.
- fetchRemoteServers, loadRemoteProjects, loadRemoteSessions — lazy load.
- openSettingsModal('remotes') — UI Add/Edit/Delete remote-сервера.

## reconnect & health probe

### Health probe per-server
remoteProbeState: Map<server_id, {timer, step, inFlight}>.
REMOTE_PROBE_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000].
REMOTE_PROBE_STEADY_INDEX = 1 (4s — interval когда сервер online).

### WS reconnect
- /ws/attach: ATTACH_WS_BACKOFFS_MS = [2s, 4s, 8s, 16s, 32s, 60s] + jitter.
- /ws/tasks: [1s, 2s, 5s, 10s]. Fallback на polling /api/tasks.
- /ws/todos: [1s, 2s, 5s, 10s]. Fallback на polling /api/todos.
- /ws/lazygit, /ws/lazydocker, /ws/telescope: manual retry через UI banner (createTuiTab.retry).

## Зависимости (CDN/embedded)
xterm.js + addon-fit + addon-web-links.
