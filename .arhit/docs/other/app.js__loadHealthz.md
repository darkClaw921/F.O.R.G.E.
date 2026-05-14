# app.js::loadHealthz

Phase 5 — Async helper. GET /healthz и заполняет state.remoteMode (bool), state.serverVersion (string|null), state.healthzLoaded (bool). При ошибке fetch/HTTP/JSON — fallback remote_mode=false и healthzLoaded=true (чтобы остальной bootstrap не зависал). Эндпоинт доступен БЕЗ Bearer-auth (см. auth::is_path_excluded на бэке). Вызывается первым в bootstrap() ДО loadActiveThemeOrNull и initTerminal — некоторые ветки рендера читают isRemoteMode() уже на первом рендере. Контракт ответа: { status, remote_mode: bool, version: string }.
