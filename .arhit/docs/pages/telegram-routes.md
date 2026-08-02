# Telegram plugin — HTTP/WS поверхность

Модуль plugins/telegram/src/routes/ (+ ws/mod.rs). Точка сборки — build_router(state: Arc<TelegramState>) -> Router: мерджит login::router() + dialogs::router() + GET /ws/telegram, применяет with_state и отдаёт Router без параметра состояния, который хост мерджит к себе через forge_telegram::register_routes.

ПОРЯДОК РЕГИСТРАЦИИ КРИТИЧЕН: register_routes вызывается в tmux-web/src/main.rs ДО .fallback(static_embed::serve_static) и ДО .layer(auth) — только тогда bearer_auth и csrf_guard автоматически покрывают /api/telegram/* и /ws/telegram в remote-mode. Без флага --telegram блок регистрации не выполняется вовсе, и GET /api/telegram/status отдаёт 404 (падает в static-fallback) — именно это фронт трактует как 'плагин выключен'.

РОУТЫ:
- GET  /api/telegram/status — 200 в любом состоянии: {enabled:true} + плоский LoginStateDto {state,phone,hint,error,user}.
- POST /api/telegram/login/start | /login/code | /login/password | /logout
- GET  /api/telegram/dialogs
- GET/POST /api/telegram/dialogs/:id/messages (?limit&before_id)
- POST /api/telegram/dialogs/:id/read | /typing
- GET  /ws/telegram (см. telegram_ws)

КОНТРАКТ ОШИБОК (routes/error.rs — ApiError + ApiErrorEnvelope + IntoResponse): {"error":{"code","message","retry_after_secs"?}}. 409 not_authorized / invalid_state; 404 unknown_dialog / no_media; 429 flood_wait (плюс стандартный заголовок Retry-After; авто-ретрай ЗАПРЕЩЁН — FLOOD_WAIT на логине измеряется часами, UI показывает countdown); 503 not_connected.

ЧЕСТНОСТЬ ЗАГЛУШЕК: пока MTProto нет, login-шаги отвечают 409 invalid_state с реальной причиной ('api_id/api_hash не настроены' либо 'нет активного соединения из состояния <kind>'), data-роуты — 409 not_authorized с текущим состоянием логина. Обобщённого 'не реализовано' и 500-ок нет ни на одном пути; матрица покрыта unit-тестами routes/mod.rs и tmux-web/tests/telegram_smoke.rs.