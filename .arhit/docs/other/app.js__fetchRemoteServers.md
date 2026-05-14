# app.js::fetchRemoteServers

Phase 5 — async. GET /api/remote-servers → state.remoteServers (массив RemoteServerView {id,label,url} без token). No-op при remoteMode=false (эндпоинт регистрируется только в remote-mode). После успешной загрузки запускает periodic health-poll через startRemoteHealthPoll() (каждые 15s). Очищает state.remoteOnline от удалённых серверов.
