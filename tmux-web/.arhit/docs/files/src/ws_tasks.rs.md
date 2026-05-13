# src/ws_tasks.rs

WebSocket-handler GET /ws/tasks?project_id=... — realtime стрим beads-задач конкретного проекта.

## Назначение
Раньше handler читал глобальный state.projects.read().active().path и подписывался на process-wide tasks_tx broadcast. Это ломалось в multi-tab/multi-project сценарии: одна вкладка переключала active project — остальные начинали получать чужие/пустые task-события.

Теперь каждое WS-соединение принимает project_id query-параметр (как ws/todos), резолвит его в путь и поднимает СВОЙ per-connection notify::RecommendedWatcher на <path>/.beads/. Snapshot и diff-broadcast — per-connection.

## Резолв project_id → path (resolve_project_path)
- Пусто/None → state.projects.read().active().path (включая transient).
- Префикс '__path__:' → strip и взять как абсолютный путь (auto-group transient проекты).
- Иначе ProjectStore::find_any(id) → path (registered + transient match).
- Если id не найден → fallback на active + warn-лог.

## Wire-протокол (server→client, Message::Text JSON)
- {kind:'snapshot', data: <br list --json --all --limit 0>} — при connect.
- {kind:'upsert', issue:{...}} — задача создана/изменена/закрыта.
- {kind:'removed', id:'...'} — задача физически удалена из beads БД.
- {kind:'reload'} — оставлено для совместимости, в per-conn режиме не используется.

## Lifecycle handle_socket
1. send snapshot (list_tasks). Если br фейл → пустой envelope {issues:[],total:0}.
2. baseline snapshot() для diff'ов.
3. find_beads_dir(project_path) — если есть, поднимаем notify::recommended_watcher на этот dir с RecursiveMode::NonRecursive. Если нет → heartbeat-only режим (клиент не реконнектится впустую, snapshot уже отдан).
4. Главный select-loop: debounce timer (DEBOUNCE_MS=200ms из tasks_watcher), notify_rx.recv, heartbeat tick (30s Ping), ws_rx.next.
5. На notify event с relevant_event() → стартуем/продлеваем debounce_deadline.
6. По истечению debounce → snapshot + diff_issues(prev,new) → serialize TaskEvent → send_text.
7. На Close/EOF → drop watcher + Best-effort close.

## Зависимости
- crate::tasks::{list_tasks, snapshot, diff_issues, TaskEvent}
- crate::tasks_watcher::{find_beads_dir, relevant_event, DEBOUNCE_MS} — публичные re-use.
- notify::RecommendedWatcher для file-watching.
- AppState.projects (read-lock на резолв path).

## Регистрация
src/main.rs роутер: .route('/ws/tasks', get(ws_tasks::tasks_ws)). Frontend подключается через static/app.js connectTasksWs() с qs ?project_id=<state.activeProjectId>.

## Что было раньше vs сейчас
| | До | После |
|--|----|------|
|project filter|нет, только active|query project_id|
|watcher|shared global tasks_tx|per-connection|
|multi-tab|broken|работает|
|нет .beads/|пустой snapshot + молчание|пустой snapshot + heartbeat|
