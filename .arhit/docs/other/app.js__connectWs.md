# app.js::connectWs

Phase 5 — connectWs(sessionName, origin). Открывает WebSocket /ws/attach?session=<name>&cols=<c>&rows=<r>[&server=<origin>] для указанной tmux-сессии. Параметр origin определяет проксирование: 'local' (или undefined) — connect к локальному devforge; <server_id> — backend прокинет WS на remote через remote_proxy. В legacy-режиме (remoteMode=false) server-param не добавляется.
