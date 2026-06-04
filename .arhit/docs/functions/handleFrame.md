# handleFrame

tmux-web/static/js/echo/ws.js — парсит входящий WS-фрейм /ws/echo и диспетчит по msg.type. Спец-обработка ДО conversation-handlers: 'ping' → сразу pong (анти idle-timeout); 'next_step_event' (broadcast, не привязан к conversation) → fetchNextSteps().then(renderSidebar) для мгновенного появления/снятия голубого свечения без ожидания 3с-поллинга. Остальные типы (assistant_chunk, assistant_done, action_buttons, notification, stats_update, autonomous_task_event, error) → state.handlers[type] (регистрируются в echo/main.js buildHandlers).
