# tmux-web/static/js/ws/tasks-ws.js

Phase 1. /ws/tasks WebSocket + fetchTasks fallback. Backoff [1s,2s,5s,10s], poll-interval 30s. Snapshot/upsert/removed/reload protocol. Origin-aware: при activeOrigin !== local/all добавляет ?server=. Управляет state.tasksData.
