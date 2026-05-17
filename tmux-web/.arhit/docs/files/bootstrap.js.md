# bootstrap.js

ES-module bootstrap entry (tmux-web/static/js/core/bootstrap.js). Экспортирует async function bootstrap() — инициализация UI после загрузки страницы.

Side-effects на module-init (top-level):
- Глобальные keydown-listeners (Cmd/Ctrl+B sidebar-toggle, Esc для закрытия mobile sidebar).
- mqlMobile change handler — переключение sidebar при изменении viewport.
- window.__forge = { groupSessionsByFolder, aggregateAllOrigins } — для регресс-тестов.

bootstrap() — последовательность:
1. loadHealthz() — определяет remoteMode + версию сервера.
2. restoreSidebarState() — восстановление состояния sidebar.
3. initTerminal + applyTerminalFontSize.
4. Подвязка обработчиков на btn-new, window-new, tab-buttons, tasks reload/new, project-select/new/settings.
5. initTuiTabs() — git/docker/telescope.
6. Если remote-mode: fetchRemoteServers + loadActiveOriginFromStorage + renderSidebar.
7. fetchProjects().finally(() => fetchSessions + startPolling + connectTasksWs + fetchTodos + connectTodosWs).
8. Best-effort fetchUserSettings() (Phase 2) — preload пользовательских настроек TODO; ошибки глотаются клиентом, UI не блокируется.
9. beforeunload — stopPolling/disconnect WS-всё.
10. visibilitychange — пауза polling при скрытой вкладке.
