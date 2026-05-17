# plugins/echo/src/lib.rs

Точка входа forge-echo crate. pub mod db, routes, state. Re-exports: HostApi, EchoConfigStub, EchoState, ServerEvent. init(cfg) async — резолвит path (cfg.db_path или ~/.config/forge/echo.db через HOME env), Db::open + migrate, возвращает Arc<EchoState>. default_db_path() — fallback ./echo.db если нет HOME. register_routes(app, state, host) привязывает host adapter к OnceCell + мерджит routes/build_router() в хост-Router. Вызывать ДО .layer(auth) чтобы /api/echo/* покрывался bearer-auth. spawn_workers — no-op до Phase 4.
