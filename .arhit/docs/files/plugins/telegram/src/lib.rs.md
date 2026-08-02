# plugins/telegram/src/lib.rs

Публичный API плагина forge-telegram: init -> register_routes -> spawn_workers -> shutdown.

init(cfg) синхронна и сетевонезависима (начальное состояние логина выводится из наличия кредов). register_routes(app, state) БЕЗ host-параметра — отдельного crate telegram-host-api нет и не планируется: плагину от хоста ничего не нужно, auth решается порядком регистрации (ДО .fallback и .layer(auth_middleware)).

spawn_workers (Фаза 3) запускает ДВА воркера: (1) updates::spawn — ВСЕГДА, даже без кредов и сессии, потому что supervisor сам ждёт Authorized и вход через wizard поднимает realtime без рестарта; (2) restore_session — только если есть и креды, и файл сессии. Оба именно ЗАПУСКАЮТСЯ: функция возвращается немедленно, медленная сеть не задерживает биндинг порта хостом.

restore_session: build_runtime -> is_authorized_with_timeout(10с) -> get_me -> Authorized, иначе Disconnected{error}; наружу ошибку не отдаёт (сервер стартует в любом случае). С Фазы 3 её же переиспользует updates-supervisor для пересборки соединения после обрыва.

shutdown: cancel -> shutdown_workers (с 2с grace на sync_update_state) -> clear_runtime (гасит MTProto-раннер и освобождает flock сессии). Идемпотентна.
