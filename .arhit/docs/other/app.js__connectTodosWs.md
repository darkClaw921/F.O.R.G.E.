# app.js::connectTodosWs

Phase 5 — Открывает WS /ws/todos. URL аналогично connectTasksWs: 'local'/'all' → ?project_id=<currentTodosProjectId()>; remote origin → ?server=<id>. При смене activeOrigin переподключается.
