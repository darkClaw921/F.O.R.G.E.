# plugins/echo/src/routes/conversations.rs

REST API для chat_sessions+messages. GET /api/echo/conversations?project_id&limit=50 → список. POST /api/echo/conversations {title, project_id?, model?='sonnet-4'} → create; если project_id не найден через HostApi::list_projects — лог warning но не блокирует (soft-FK). DELETE /api/echo/conversations/:id → 204 (идемпотентно, cascade messages). GET /api/echo/conversations/:id/messages?limit=200&before — 404 если нет сессии (UX лучше), иначе list_by_session ASC. POST /api/echo/conversations/:id/delete — legacy. ApiError JSON {error}.
