# PortalFanIn

Мультиплексор broadcast-каналов всех порталов Bitrix24 для одного WS-соединения (plugins/bitrix/src/ws/mod.rs, мультипортальность Ф5).

ЗАЧЕМ. /ws/bitrix стал ОДНИМ сокетом на ВСЕ порталы: панель показывает строку вкладок, и бейдж соседней вкладки обязан подниматься без второго соединения. PortalFanIn держит по одному broadcast::Receiver на каждый слот реестра и отдаёт кадры циклу сокета.

МЕТОДЫ.
- new(&Arc<B24Registry>) — подписывается на broadcast КАЖДОГО слота, включая ещё не занятый (его автомат вещает login_state для экрана подключения). Вызывать ДО отправки снапшота.
- states() -> Vec<Arc<B24State>> — состояния, на которые есть подписка, в порядке реестра (для снапшота login_state).
- sync(&Arc<B24Registry>) -> Vec<Arc<B24State>> — приводит набор подписок к текущему составу реестра и возвращает ТОЛЬКО новые состояния: их снапшот вызывающий шлёт уже после подписки. Сравнение по Arc::ptr_eq, а не по portal_id: пустой слот идентификатора не имеет, а занятый меняет его на живом процессе.
- recv() -> (usize, String, Result<B24ServerEvent, RecvError>) — ждёт кадр от любого портала через futures_util::future::select_all; cancel-safe (broadcast::Receiver::recv не теряет кадр при отмене), пустой набор ждёт вечно.
- portal_id(idx), drop_at(idx), len(), is_empty().

ПОЧЕМУ НЕ ЗАДАЧИ-ПЕРЕСЫЛКИ. (1) Каденс: ws_subscribers() — это receiver_count() портала, приёмник внутри задачи пережил бы закрытие сокета и держал бы портал в active-каденсе с закрытой панелью. (2) Lagged принадлежит конкретному приёмнику; общий mpsc отставал бы сам и resync перестал бы быть адресным.

СВЯЗИ: B24Registry::list/subscribe_composition, B24State::subscribe/ws_subscribers, ws::broadcast_frame, ws::frame_portal_id, protocol::B24Frame. Тесты — plugins/bitrix/tests/ws_fanin.rs.
