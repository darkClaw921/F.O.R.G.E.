# tmux-web/static/js/echo/ws.js

Echo WebSocket клиент. connectEchoWs(conversationId, handlers) — открывает /ws/echo с withWsToken auth, регистрирует handlers map для server-side messages (assistant_chunk, assistant_done, action_buttons, notification, stats_update, autonomous_task_event, error, ping). На server ping автоматически шлёт pong. Reconnect: backoff [1s,2s,5s,10s]. disconnectEchoWs — закрывает, отключает реконнект. Sender helpers: sendUserMessage/sendCancel/sendActionInvoke сериализуют ClientMsg и шлют через ws.send.
