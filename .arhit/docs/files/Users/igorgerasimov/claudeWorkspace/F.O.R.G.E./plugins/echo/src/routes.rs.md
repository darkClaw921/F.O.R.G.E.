# /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./plugins/echo/src/routes.rs

routes.rs — HTTP-роуты плагина Echo. Phase 1: только GET /api/echo/healthz → 200 'ok'. Функция build_router(Arc<EchoState>) -> Router создаёт Router::new().route(...).with_state(state). Префикс /api/echo/* гарантирует отсутствие коллизий с хост-routes. В Phase 2-5 будут добавлены /api/echo/conversations, /api/echo/memories, /api/echo/stats, /api/echo/autonomous-tasks и /ws/echo.
