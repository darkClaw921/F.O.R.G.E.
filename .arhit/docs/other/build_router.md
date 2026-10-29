# build_router

plugins/bitrix/src/routes/mod.rs — сборка Router плагина Bitrix24.

СИГНАТУРА (изменена в Ф3 мультипортальности)
build_router(registry: Arc<B24Registry>) -> Router
Раньше принимала Arc<B24State>. Возвращает Router без параметра state — его сразу мерджит хост (app.merge(build_router(registry))).

ПОЧЕМУ РЕЕСТР, А НЕ ПОРТАЛ
Роутер собирается один раз при старте, а состав реестра меняется на живом процессе (B24Registry::add / remove, Ф4). Привязка через with_state к конкретному Arc<B24State> пережила бы отключение своего портала. Портал теперь выбирает каждый запрос — экстрактором routes::Portal.

Единственное, что по-прежнему фиксируется при сборке, — max_upload_bytes для слоя DefaultBodyLimit на /api/bitrix/upload: он берётся из registry.shared(), одинаков у всех порталов по построению (BitrixRootConfig::portal_configs раздаёт общие настройки копией) и обязан быть известен до появления запроса.

СОСТАВ
GET /api/bitrix/status + GET /ws/bitrix, дальше .merge() семи суб-роутеров: login, dialogs, analysis, tasks, media, voice, upload(max_upload_bytes). Все они теперь Router<Arc<B24Registry>>. Завершается .with_state(registry).

ВЫЗЫВАЕТСЯ из forge_bitrix::register_routes — ОБЯЗАТЕЛЬНО до .fallback(...) и .layer(auth_middleware) хоста, иначе 404 на роуты плагина или дыра в авторизации.

ТЕСТОВЫЕ ФИКСТУРЫ
Для одного портала: build_router(B24Registry::single(state)). Для нескольких: B24Registry::from_states(vec![...]).
