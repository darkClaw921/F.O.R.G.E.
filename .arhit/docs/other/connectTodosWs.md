# connectTodosWs

Phase 4 — открывает WebSocket /ws/todos?project_id=<pid> (tmux-web/static/app.js). No-op если уже OPEN/CONNECTING. Парные функции: disconnectTodosWs, scheduleTodosWsReconnect, handleTodosWsMessage. Backoff серия [1000, 2000, 5000, 10000]ms. На onclose (если не closedByUs) запускает fallback polling и schedule reconnect. handleTodosWsMessage диспатчит kind: snapshot (state.todosData = msg.todos; renderTasks), upsert (replace или unshift по id), removed (splice по id), reload (fetchTodos()). Аналог connectTasksWs из Phase 6.D.
