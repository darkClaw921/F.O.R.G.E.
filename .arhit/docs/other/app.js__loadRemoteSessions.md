# app.js::loadRemoteSessions

Phase 5 — async. GET /api/sessions?server=<id> → state.remoteSessions[serverId]. Lazy-load (см. loadRemoteProjects). Бэкенд прокси добавит origin=<serverId> к каждой записи через remote_proxy::enrich_with_origin.
