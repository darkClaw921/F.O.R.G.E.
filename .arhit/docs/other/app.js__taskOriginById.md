# app.js::taskOriginById

Phase 5 — Ищет issue по id в state.tasksData.issues и возвращает его origin. Используется в closeTask/reopenTask/updateTask для решения — проксировать запрос или нет. Fallback 'local' если задача не найдена в snapshot'е.
