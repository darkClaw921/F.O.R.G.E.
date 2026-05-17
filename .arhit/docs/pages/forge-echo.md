# forge-echo plugin

Echo плагин (Э.Х.О) — встроенный чат-ассистент F.O.R.G.E. на базе Claude CLI.
Оформлен как отдельный крейт в Cargo workspace (path: plugins/echo).

## Архитектура

- **Crate**: plugins/echo (forge-echo)
- **Public API** (lib.rs):
  - fn init(EchoConfigStub) -> Arc<EchoState>
  - fn register_routes(Router, Arc<EchoState>, Arc<dyn HostApi>) -> Router
  - fn spawn_workers(&Arc<EchoState>, Arc<dyn HostApi>) — no-op в Phase 1
- **State** (state.rs):
  - EchoState { host: Arc<OnceCell<Arc<dyn HostApi>>>, broadcast: broadcast::Sender<ServerEvent> }
  - EchoConfigStub — placeholder, заменится на EchoConfig в Phase 6
  - ServerEvent enum — placeholder, варианты появятся в Phase 3
- **Routes** (routes.rs):
  - GET /api/echo/healthz → 200 'ok'

## Plugin boundary

Плагин зависит ТОЛЬКО от echo-host-api (trait HostApi + DTO). Не знает про
tmux-web::AppState, projects::ProjectStore, tmux::* напрямую — это обеспечивает
изоляцию и тестируемость с мок-хостом.

## Registration в tmux-web

В main.rs ~line 487, ДО .layer(auth_middleware) и .fallback:

    let echo_cfg = forge_echo::EchoConfigStub::default();
    let echo_state = forge_echo::init(echo_cfg)?;
    let echo_host: Arc<dyn HostApi> = Arc::new(EchoHostAdapter { state: app_state.clone() });
    let app = forge_echo::register_routes(app, echo_state.clone(), echo_host);

Порядок критичен: register_routes ДО layer, чтобы Bearer-auth покрывал
/api/echo/* и (в будущем) /ws/echo автоматически в remote-mode.

## Будущие фазы

- Phase 2: SQLite + migrations + memories/conversations CRUD
- Phase 3: Claude CLI integration + WS streaming
- Phase 4: Autonomous tasks scheduler
- Phase 5: Frontend (Echo UI) + memory automation + action buttons
- Phase 6: Settings/config + hardening + tests