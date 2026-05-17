Phase 1: Полная миграция tmux-web/static/app.js (6743 строки IIFE) на нативные ES Modules.

## Маппинг функций → новые модули

### core/ (Phase 0)
- state, AUTH_TOKEN_KEY → js/core/state.js
- DOM refs ($layout, $sidebar, …) → js/core/dom.js
- bootstrapAuthToken, getAuthToken, fetch override, withWsToken → js/core/auth.js
- escapeHtml/escapeAttr/escapeText, buildModalOverlay, detectClientOS, copyToClipboardSafe, fallbackCopy → js/core/utils.js
- apiFetch, withServerParam, parseGlobalId, formatGlobalId, dtoOrigin → js/core/api.js
- _mqlMobile, isMobileViewport, TERM_FONT_SIZE_*, applyTerminalFontSize → js/core/viewport.js

### terminal/
- initTerminal, sendResize, scheduleResizeFromTerm, setStatus, showPlaceholder → js/terminal/xterm.js
- mapTermTheme → js/terminal/theme-mapper.js

### ws/
- connectWs, disconnectWs, scheduleAttachWsReconnect, handleControlFromServer → js/ws/attach.js
- connectTasksWs, disconnectTasksWs, scheduleTasksWsReconnect, handleTasksWsMessage, fetchTasks, startTasksPolling/stopTasksPolling, setTasksStatus → js/ws/tasks-ws.js
- connectTodosWs, disconnectTodosWs, scheduleTodosWsReconnect, handleTodosWsMessage, fetchTodos, startTodosPolling/stopTodosPolling → js/ws/todos-ws.js

### sessions/
- fetchSessions, buildSessionItem, groupSessionsByFolder, startPolling/stopPolling, createSessionPrompt, renameSession, killSession, openSession, switchSession → js/sessions/sessions.js
- fetchWindows, renderWindowBar, selectWindow, createWindow, killWindow, renameWindow, startWindowsPolling/stopWindowsPolling → js/sessions/windows.js

### sidebar/
- renderSidebar, renderSidebarWithOrigin, renderOriginSection → js/sidebar/sidebar.js
- renderOriginTabs, getCollapsedOrigins/isOriginCollapsed/toggleOriginCollapsed/persistCollapsedOrigins, loadActiveOriginFromStorage/saveActiveOriginToStorage → js/sidebar/origin-tabs.js
- applySidebarCollapsed, setMobileSidebarOpen, toggleSidebar, restoreSidebarState → js/sidebar/mobile.js

### tabs/
- switchTab → js/tabs/tabs.js
- createTuiTab, initTuiTabs, INSTALL_ENTRIES (lazygit/lazydocker/telescope), getActiveProject, mountGitTerm/openLazygitForActiveProject/connectGitWs/closeGitWs/gitSwitchCwd/showGitBanner/hideGitBanner/retryGitConnection, sendToActivePty → js/tabs/tui-tabs.js

### tasks/
- TASK_COLUMNS, COLUMN_TITLES, CLOSED_LIMIT, renderTasks, compareIssues, renderColumn, renderTodoCard, renderCard, currentTodosProjectId → js/tasks/render.js
- TASK_EDIT_STATUSES, TASK_TYPES, buildTaskFormHtml, openCreateModal, openEditModal, openTodoEditModal → js/tasks/modals.js
- getIssueIndex, applyOptimisticPatch, rollbackIssue, createTask, updateTask, taskOriginById, closeTask, reopenTask, promoteTodo → js/tasks/crud.js

### projects/
- fetchProjects, renderProjectSelect, switchActiveProject → js/projects/projects.js
- openNewProjectModal → js/projects/new-project.js

### settings/
- openSettingsModal → js/settings/modal.js
- renderRemotesTable, openEditRemoteRow → js/settings/remotes-tab.js
- buildNotificationsForm, saveProjectSettings → js/settings/notifications-tab.js

### themes/
- applyTheme, switchTheme, loadActiveThemeOrNull, THEME_UI_KEYS/THEME_TERM_BASE_KEYS/THEME_TERM_ANSI_KEYS/THEME_TERM_KEYS, HEX_COLOR_RE, normalizeHex, cloneThemeColors, validateDraft, buildThemePayload → js/themes/api.js
- loadThemesIntoPanel, renderThemesPanel, buildThemeCard → js/themes/panel.js
- openThemeEditor, buildColorPickerRow, buildLivePreviewContainer → js/themes/editor.js

### remote/
- loadHealthz, isRemoteMode → js/remote/healthz.js
- state.remoteOnline (runtime init), REMOTE_PROBE_BACKOFFS_MS, fetchRemoteServers, loadRemoteProjects/Sessions, probeRemoteServer, startRemoteHealthPoll/stopRemoteHealthPoll, aggregateAllOrigins → js/remote/servers.js

### Entry
- bootstrap() + top-level keydown/mqlMobile listeners + window.__forge → js/core/bootstrap.js
- window.ForgeApp = { sendToActivePty, state } → js/public-api.js
- import './core/auth.js' → import './public-api.js' → bootstrap() → js/main.js

## index.html
`<script src=/app.js>` → `<script type=module src=/js/main.js>`. quick-cmd.js и hotkeys.js остаются классическими.

## app.js
Опустошён до одного комментария (cache-warmth для старых вкладок).

## Smoke
Все 30+ модулей отдаются HTTP 200 (content-type text/javascript). node --check проходит везде. cargo build green. Контракт window.ForgeApp.{sendToActivePty,state} сохранён 1:1.

## Известное legacy-поведение
В createTuiTab() (tabs/tui-tabs.js): mapTermTheme(state.activeTheme) — передаётся ВЕСЬ theme-объект (а не theme.term). Это копия legacy app.js:2103-2104 — bug сохранён 1:1 как требует pure refactor. Fallback fallbackTheme сработает (т.к. mapTermTheme(theme) вернёт объект с undefined полями).