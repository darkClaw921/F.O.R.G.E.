# main.rs

Главный entry-point devforge.

Phase 1 (remote mode):
- main() парсит CLI через cli::parse() и обрабатывает Mode::Start/Stop/Status немедленно (daemon-команды).
- Для Mode::Run(opts): загружает server_config.json через server_config::load() (Err — печатает warning, продолжает с CLI-only), затем server_config::resolve(&opts, file_cfg) даёт EffectiveConfig (bind/port/auth_token/remote_mode по приоритетам CLI > file > env > default). server_config::finalize_token(&effective) возвращает финальный токен: при remote_mode=true без существующего — генерирует 64-hex и сохраняет в файл, печатает банер.
- AppState: дополнено полями remote_mode: bool и auth_token: Arc<Option<String>>. Передаётся в healthz через State<AppState>.
- Router сначала строится без auth, после .with_state — условно навешивает axum::middleware::from_fn_with_state(auth::AuthState::new(Some(token)), auth::bearer_auth) ТОЛЬКО если auth_token=Some. Path-exclusion (/healthz, /, /assets/*, /static/*) — внутри bearer_auth.
- Финальный bind: при remote_mode=false hardcoded SocketAddr::from(([127,0,0,1], port)) (legacy invariant); при remote_mode=true — <bind_host>:<port>.

healthz handler:
- Принимает State<AppState>.
- Возвращает Json(HealthzResponse{status:'ok', remote_mode: state.remote_mode, version: env!('CARGO_PKG_VERSION')}).
- Content-Type автоматически application/json (axum::Json).
- Покрыт unit-тестами healthz_response_shape_remote_mode_off/on (mod tests внизу файла).

Не меняет: ни одного существующего endpoint'а или DTO; добавляет только новые поля в AppState и заменяет return string на JSON-структуру в healthz.
