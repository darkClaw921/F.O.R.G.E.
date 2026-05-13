# connectTasksWs

JS-функция в app.js. Открывает WebSocket к /ws/tasks. Если уже OPEN/CONNECTING → no-op. На onopen сбрасывает tasksWsBackoffStep, останавливает fallback polling, сообщает 'tasks: live'. handleTasksWsMessage обрабатывает kind='snapshot'/'upsert'/'removed'/'reload'. На onclose — schedule reconnect через TASKS_WS_BACKOFFS_MS=[1000,2000,5000,10000] и startTasksPolling fallback.
