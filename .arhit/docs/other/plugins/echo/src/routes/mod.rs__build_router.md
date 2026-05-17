# plugins/echo/src/routes/mod.rs::build_router

Сборщик Router плагина. Регистрирует: /api/echo/healthz, memories router, conversations router, stats router (Phase 3), /ws/echo handler (Phase 3). Использует with_state(Arc<EchoState>). Routes регистрируются ДО bearer-auth middleware host'а — auth покрывает /api/echo/* и /ws/echo автоматически.
