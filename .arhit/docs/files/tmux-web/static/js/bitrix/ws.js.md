# tmux-web/static/js/bitrix/ws.js

WebSocket-клиент /ws/bitrix панели «Битрикс24». Тонкая надстройка над общим слоем js/messenger/ws-core.js::createWsClient — ядро держит соединение, лестницу реконнекта, ping/pong и оценку задержки; этот модуль знает только про адрес, токен и куда класть результат. Копия ядра ради третьей панели не заводится: оно уже покрыто Node-тестом tests/frontend/messenger_ws_core.test.mjs.

Соединение ПОСТОЯННОЕ и НЕ привязано к вкладке devforge: сервер вещает события без фильтрации по диалогу, потому что бейдж непрочитанного на кнопке Ⓑ и тосты обязаны работать, пока пользователь сидит в terminal/tasks. Teardown при смене вкладки не делается — только visibilitychange и beforeunload через core/bootstrap.js.

Константы: WS_BACKOFFS_MS = [1000, 2000, 5000, 10000] (последнее повторяется), PING_INTERVAL_MS = 15000 (эталон HEARTBEAT_INTERVAL из plugins/bitrix/src/ws/mod.rs — расхождение ухудшит точность LAT, но ничего не сломает), SERVER_FRAMES — полный список 16 кадров сервера.

SERVER_FRAMES обязан дословно совпадать с тестом server_frame_tags_are_frozen в plugins/bitrix/src/ws/protocol.rs; сверка автоматизирована в tests/frontend/bitrix_contract.test.mjs (сравнение текстовое: ws.js импортирует core/auth.js, который на верхнем уровне трогает localStorage, и в голом Node не грузится).

URL: withWsToken(ws(s)://host/ws/bitrix) — токен в query, потому что браузерный WebSocket не умеет заголовок Authorization.

Маршрутизация onFrame: ping и resync имеют отдельные колбэки, остальное ищется в карте handlers; неизвестный тип — console.debug, не ошибка (сервер может начать присылать новый кадр раньше, чем обновится закешированный фронт). Исключение обработчика ловится в callHandler и не рвёт соединение.

Экспорт: connectB24Ws(handlers) (идемпотентен — при живом сокете только обновляет handlers), disconnectB24Ws(), isB24WsOpen(), _debugWsState().

ФАЗА 5. Кадры message_new и message_update несут MessageDto — тот же тип, что отдаёт страница истории (ws/protocol.rs::MessagePayload стал алиасом на dto::MessageDto). Набор SERVER_FRAMES не изменился: форма кадров заморожена с Фазы 1, и сверку с protocol.rs делает bitrix_contract.test.mjs.
