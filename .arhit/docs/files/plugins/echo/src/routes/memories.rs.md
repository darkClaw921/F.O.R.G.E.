# plugins/echo/src/routes/memories.rs

REST API для memories: GET /api/echo/memories?scope&project_id&day (валидирует scope enum), POST /api/echo/memories {scope, project_id?, day?, content, source?=manual} → upsert (всегда 200), PATCH /api/echo/memories/:id {content} → 204 либо 404, DELETE /api/echo/memories/:id → 204 либо 404, GET /api/echo/memories/by-id/:id, и legacy POST :id/patch, DELETE :id/delete. ApiError(StatusCode, String) сериализует JSON {error: ...}. router() мерджится в routes/mod.rs::build_router и проходит через bearer-auth middleware хоста.
