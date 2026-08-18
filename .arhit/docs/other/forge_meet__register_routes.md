# forge_meet::register_routes

Монтирует HTTP- и WS-поверхность плагина в роутер хоста. Вызывается ДО .fallback(...) и ДО .layer(auth_middleware): регистрация после fallback дала бы 404 на все роуты плагина, а после layer — дыру в авторизации (Bearer-auth и csrf_guard хоста должны покрывать роуты плагина автоматически в remote-mode). Таблица путей заморожена в шапке routes/mod.rs и сверяется с фронтом (static/js/meet/api.js) контрактным тестом в обе стороны.
