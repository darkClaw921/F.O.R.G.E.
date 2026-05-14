# healthz

Async-handler для GET /healthz (Phase 1).

Сигнатура: async fn healthz(State(state): State<AppState>) -> Json<HealthzResponse>.

Возвращает {status:'ok', remote_mode: state.remote_mode, version: CARGO_PKG_VERSION}.

Доступен без Bearer-токена (auth::EXCLUDED_EXACT). Используется:
- Frontend'ом до получения токена в remote-mode (читает remote_mode для рендера UI логина).
- Готовностью сервера (HTTP 200 при работающем axum-стек).

Изменение от Phase 0: раньше возвращал статическую строку 'ok' (text/plain). Теперь — JSON. Контракт изменения зафиксирован в Phase 1.
