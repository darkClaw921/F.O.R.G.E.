# tmux-web/static/js/ws/tasks-ws.js

Tasks WebSocket клиент + REST fetch. Реализует /ws/tasks с auto-reconnect (exponential backoff TASKS_WS_BACKOFFS_MS), poll-fallback (TASKS_POLL_INTERVAL_MS=30s) и хэндлинг snapshot/upsert/removed/reload.

## Tasks следуют за cwd сессии (как git-вкладка)

Главное правило: задачи в UI всегда отражают cwd активной tmux-сессии (state.currentSession), а НЕ activeProjectId. Это сделано симметрично с git-вкладкой (см. tabs/tui-tabs.js::syncGitToCurrentSession).

- sessionCwdOrNull() — берёт state.sessions[?currentSession].path, возвращает абсолютный путь или null.
- connectTasksWs() — собирает project_id как __path__:<cwd>, если cwd известен. Иначе fallback на state.activeProjectId. ws_tasks.rs::resolve_project_path распознаёт префикс __path__: и резолвит путь напрямую (минуя ProjectStore). state.tasksCurrentCwd хранит cwd текущей подписки.
- fetchTasks() — REST /api/tasks?path=<cwd>, если cwd известен. Сервер (main.rs::get_tasks) при наличии ?path использует его как cwd для list_tasks (br list), иначе fallback на active project.
- syncTasksToCurrentSession() — вызывается из sessions.js::openSession и switchSession после установки state.currentSession. Если cwd не изменился — no-op. Иначе чистит tasksData, disconnect WS, и (если активна tab=tasks) делает fetchTasks + connectTasksWs.

## handleTasksWsMessage(raw)
JSON-сообщения от сервера: snapshot (полная замена), upsert (добавить/обновить issue по id), removed (удалить), reload (форс-фетч). Невалидный JSON и неизвестные kind логируются и игнорируются.

## setTasksStatus(kind, text)
Пишет в $tasksStatus.textContent. Используется для индикации live/reconnect/error состояний.
