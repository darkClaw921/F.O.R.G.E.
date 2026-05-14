# HealthzResponse

Struct в main.rs (Phase 1). JSON-ответ для GET /healthz.

Поля:
- status: &'static str — всегда 'ok' (зарезервировано для будущих degraded-состояний).
- remote_mode: bool — true если сервер в remote-mode (--remote/server_config.json подразумевает remote); используется frontend'ом для рендера UI логина.
- version: &'static str — env!('CARGO_PKG_VERSION'), e.g. '0.1.3'.

Контракт:
- Доступен без Bearer-token (см. auth::EXCLUDED_EXACT). Frontend должен иметь возможность прочитать remote_mode ДО логина.
- Content-Type: application/json (через axum::Json wrapper).

Unit-tests: healthz_response_shape_remote_mode_off / _on в main.rs::tests.
