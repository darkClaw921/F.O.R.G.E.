# /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./plugins/echo/src/lib.rs

forge-echo lib.rs — точка входа плагина Echo. Публичный API: init(EchoConfigStub) -> anyhow::Result<Arc<EchoState>>, register_routes(Router, Arc<EchoState>, Arc<dyn HostApi>) -> Router, spawn_workers(&Arc<EchoState>, Arc<dyn HostApi>) (Phase 4+). Re-exports: HostApi, EchoConfigStub, EchoState, ServerEvent. Phase 1: init создаёт EchoState с broadcast(256), register_routes привязывает host в OnceCell и мерджит routes::build_router(state) с переданным app. spawn_workers — no-op.
