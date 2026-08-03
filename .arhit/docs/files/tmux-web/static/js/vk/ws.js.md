# tmux-web/static/js/vk/ws.js

WebSocket-клиент панели VK (Ф3): надстройка над messenger/ws-core.js.

Ядро держит соединение, лестницу реконнекта, ping/pong и оценку задержки; этот модуль знает адрес (/ws/vk + withWsToken из core/auth.js), раскладывает кадры по обработчикам панели и кладёт сокет с задержкой в vkState.

Экспорт: connectVkWs(handlers), disconnectVkWs(), isVkWsOpen(), _debugWsState(), WS_BACKOFFS_MS, PING_INTERVAL_MS, SERVER_FRAMES.

Соединение ПОСТОЯННОЕ и не привязано к вкладке devforge: сервер вещает события без фильтрации по беседе, потому что бейдж непрочитанного на кнопке Ⓥ и тосты обязаны работать, пока пользователь сидит в terminal/tasks. Закрывается только по visibilitychange:hidden и beforeunload.

Кадры сервера (15 штук, контракт plugins/vk/src/ws/protocol.rs): login_state, new_message, message_edited, message_deleted, typing, online, read, upload_progress, download_progress, notify_settings, unread_total, history_cleared, dialog_changed, resync, ping. Клиент шлёт только pong (вся мутация идёт по REST — её проще авторизовать, логировать и ретраить). Неизвестный тип кадра не ошибка: сервер может начать присылать новое событие раньше, чем обновится закешированный фронт.

Инвариант снятия обработчиков перед close() живёт в ядре и покрыт тестом там; disconnectVkWs делегирует.
