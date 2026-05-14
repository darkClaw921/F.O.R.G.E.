# app.js::connectTasksWs

Phase 5 — Открывает WS /ws/tasks. URL формируется в зависимости от state.activeOrigin: 'local'/'all' (или legacy) → ?project_id=<activeProjectId>; <server_id> → ?server=<id> (без project_id, т.к. он на remote-стороне). Бэкенд прокси прокинет WS на remote и пересылает snapshot/upsert/removed/reload кадры. При смене activeOrigin (через origin-табы) connectTasksWs вызывается заново.
