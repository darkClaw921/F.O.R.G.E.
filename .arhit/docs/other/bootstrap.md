# bootstrap

Async entry-point tmux-web frontend (tmux-web/static/app.js, обновлён в Phase 3 wk7).

## Что делает
async bootstrap() — точка входа, регистрируется через document.addEventListener('DOMContentLoaded', bootstrap).

Последовательность:
1. **await loadActiveThemeOrNull()** (Phase 3 wk7) — загружает /api/themes/active, применяет CSS-переменные на :root, возвращает xterm ITheme или null.
2. **initTerminal(termTheme)** — создаёт xterm Terminal с темой (или fallback при null).
3. showPlaceholder(true), setStatus('disconnected') — initial UI.
4. Регистрирует DOM-листенеры:  → createSessionPrompt, tab-bar (Phase 6.A), / (Phase 6.A/C), // (Phase 6.B).
5. fetchProjects().finally(() => { fetchSessions(); startPolling(); connectTasksWs(); fetchTodos(); connectTodosWs(); }).
6. window.addEventListener('beforeunload', ...) — stopPolling + disconnect всех WS.
7. document.addEventListener('visibilitychange', ...) — пауза/возобновление polling при скрытии страницы.

## Почему async (Phase 3 wk7)
До Phase 3 bootstrap был sync. Загрузка темы обязана произойти ДО new Terminal (xterm рендерит фон при open() и присвоение options.theme после этого не пересчитывает background-canvas). await loadActiveThemeOrNull() — единственный новый async-шаг; остальные init-ы (fetchProjects.finally(...)) сохраняют прежнее не-блокирующее поведение.

DOMContentLoaded листенер не ждёт возврата bootstrap — но это безопасно, т.к. внутри bootstrap await цепочка обеспечивает правильный порядок.

## Связанные
- loadActiveThemeOrNull, applyTheme, mapTermTheme — Phase 3 wk7.
- initTerminal — теперь принимает termTheme.
- fetchProjects, fetchSessions, startPolling — sessions/projects.
- connectTasksWs, fetchTodos, connectTodosWs — realtime.
- DOMContentLoaded регистрация в конце IIFE (~строка 2880).
