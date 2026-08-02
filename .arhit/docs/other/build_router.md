# build_router

ВНИМАНИЕ: имя build_router в проекте неоднозначно — так называются две разные функции сборки роутеров плагинов:

1) plugins/echo/src/routes/mod.rs::build_router(state: Arc<EchoState>) -> Router — /api/echo/* + /ws/echo.
2) plugins/telegram/src/routes/mod.rs::build_router(state: Arc<TelegramState>) -> Router — /api/telegram/* + /ws/telegram; подробности см. на странице документации 'telegram-routes'.

Общий контракт обеих: возвращают Router с уже применённым with_state, вызываются из register_routes своего плагина и мерджатся в приложение хоста ДО .fallback(...) и .layer(auth), чтобы Bearer-auth и csrf_guard покрывали роуты плагина в remote-mode.
