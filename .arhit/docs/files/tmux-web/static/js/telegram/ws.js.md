# tmux-web/static/js/telegram/ws.js

WebSocket-клиент /ws/telegram (Фаза 3), порт js/echo/ws.js. Соединение ПОСТОЯННОЕ и НЕ привязано к вкладке: сервер вещает события без фильтрации по диалогу, потому что unread-бейдж на ✈ и тосты обязаны работать из terminal/tasks.

connectTelegramWs(handlers) идемпотентен — при живом сокете только обновляет handlers (второй сокет не создаётся). disconnectTelegramWs снимает onclose/onerror/onmessage/onopen ДО close(): иначе отложенный onclose уже мёртвого сокета обнуляет состояние нового и рождает reconnect-loop (реальный дефект из echo/ws.js). isTelegramWsOpen()/_debugWsState() — для статусбара и e2e.

Backoff [1,2,5,10]с (последнее значение повторяется), сброс при onopen. URL собирается через withWsToken (в remote-mode токен идёт в query: конструктор WebSocket не умеет заголовки). Серверный ping -> МГНОВЕННЫЙ pong (HEARTBEAT 15с, IDLE_TIMEOUT 60с) + latencyFromPingDrift -> tgState.lat + колбэк lat. resync -> отдельный колбэк (полный перезапрос состояния по REST). Неизвестный type -> console.debug без исключения (forward-compat: сервер может начать слать новые события раньше, чем обновится закешированный фронт).
