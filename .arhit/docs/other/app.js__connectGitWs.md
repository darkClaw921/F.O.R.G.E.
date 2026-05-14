# app.js::connectGitWs

Phase 5 — connectGitWs(cwd). Открывает WebSocket /ws/lazygit?cwd=<cwd>&cols=<c>&rows=<r>[&server=<state.activeOrigin>]. server-param добавляется только если isRemoteMode() и activeOrigin != 'local'/'all'. Бэкенд при наличии &server проксирует WS на remote devforge.
