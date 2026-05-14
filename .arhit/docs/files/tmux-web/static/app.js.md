# tmux-web/static/app.js

Frontend tmux-web (xterm + WS-attach + tasks/git/projects/themes/remote).

## Сборка
Один большой IIFE-modul (~5700 строк) в tmux-web/static/app.js. Embedded в бинарь через rust-embed.

## Ключевые блоки

### state (объект)
Центральное хранилище в памяти. Поля:
- ws, currentSession, sessions, term, fitAddon — основной /ws/attach + xterm.
- attachWsBackoffStep / attachWsReconnectTimer / attachWsClosedByUs / attachWsOrigin (Phase 7) — backoff-reconnect /ws/attach.
- activeTab ('terminal'|'tasks'|'git'), tasksData, tasksWs (+backoff/timer/closedByUs), todosData/todosWs.
- projects, activeProjectId, projectFilter — мульти-проекты.
- remoteMode, serverVersion, healthzLoaded — Phase 5: /healthz bootstrap.
- remoteServers (Array<RemoteServerView>), remoteOnline (Map<id, 'online'|'offline'|'unknown'>) — Phase 5/7.
- remoteSessions/remoteProjects (Map<server_id, ...>) — Phase 6 aggregated view.
- activeOrigin ('all'|'local'|server_id) — Phase 5/6 origin-фильтр.
- gitTerm { term, fit, ws, mounted, currentCwd, errorSticky } — изолированный xterm-context lazygit-таба.
- activeTheme — Phase 3 themes.

### Ключевые функции
- bootstrap() — loadHealthz, тема, initTerminal, sidebar, project bar, WS connect.
- connectWs(sessionName, origin) — /ws/attach с ?server=<origin>. Phase 7: backoff reconnect.
- disconnectWs() — Phase 7: помечает attachWsClosedByUs, чтобы onclose не реконнектился.
- scheduleAttachWsReconnect() (Phase 7) — backoff серии ATTACH_WS_BACKOFFS_MS=[2s,4s,8s,16s,32s,60s] + jitter(0..1s).
- connectTasksWs / connectTodosWs / connectGitWs — WS подписки.
- scheduleTasksWsReconnect / scheduleTodosWsReconnect — backoff для tasks/todos.
- probeRemoteServer(serverId) (Phase 7) — per-server health probe с экспоненциальным backoff.
- startRemoteHealthPoll/stopRemoteHealthPoll (Phase 7) — sync remoteProbeState с state.remoteServers.
- renderSidebar / renderOriginSection — origin-табы и группировка сессий.
- isRemoteMode() — guard для UI-features remote-режима.
- fetchRemoteServers, loadRemoteProjects, loadRemoteSessions — lazy load.
- openSettingsModal('remotes') — UI Add/Edit/Delete remote-сервера.

## Phase 7 — reconnect & health probe

### Health probe per-server
remoteProbeState: Map<server_id, {timer, step, inFlight}>.
REMOTE_PROBE_BACKOFFS_MS = [2000, 4000, 8000, 16000, 32000, 60000].
REMOTE_PROBE_STEADY_INDEX = 1 (4s — interval когда сервер online).
REMOTE_PROBE_JITTER_MAX_MS = 1000.
Алгоритм:
1. fetch /api/remote-servers/:id/healthz.
2. ok && data.online → status='online', step=STEADY_INDEX.
3. !ok / network error → status='offline', step++.
4. setTimeout next probe через backoffs[step] + jitter(0..1s).
5. На смену remoteOnline-статуса вызывает renderSidebar() (offline-badge).

### WS reconnect
- /ws/attach: ATTACH_WS_BACKOFFS_MS = [2s, 4s, 8s, 16s, 32s, 60s] + jitter. На onopen → step=0. Сохраняет currentSession+origin.
- /ws/tasks: TASKS_WS_BACKOFFS_MS = [1s, 2s, 5s, 10s]. Fallback на polling /api/tasks.
- /ws/todos: TODOS_WS_BACKOFFS_MS = [1s, 2s, 5s, 10s]. Fallback на polling /api/todos.
- /ws/lazygit: manual retry через UI banner.

## Зависимости (CDN/embedded)
xterm.js + addon-fit + addon-web-links.
