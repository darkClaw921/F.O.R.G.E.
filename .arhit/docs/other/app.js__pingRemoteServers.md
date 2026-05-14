# app.js::pingRemoteServers

Phase 5 — async. Параллельно пингует GET /api/remote-servers/:id/healthz по всем зарегистрированным remote-серверам и обновляет state.remoteOnline (Map: id → 'online'|'offline'|'unknown'). Если хоть один статус изменился — вызывает renderSidebar для перерисовки индикаторов. Network/HTTP-ошибка → offline. Запускается periodic через startRemoteHealthPoll (15s интервал).
