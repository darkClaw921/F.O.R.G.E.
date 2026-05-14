# app.js::loadRemoteProjects

Phase 5 — async. GET /api/projects?server=<id> → state.remoteProjects[serverId]. Lazy-load: вызывается только при разворачивании origin-секции в sidebar или при выборе сервера в origin-табах. Кэш через Map. При ошибке кладёт пустой массив.
