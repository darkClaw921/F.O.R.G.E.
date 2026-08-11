# tmux-web/static/js/vk/ws.js

WebSocket-клиент /ws/vk: тонкая надстройка над js/messenger/ws-core.js (адрес, токен, раскладка кадров по обработчикам панели).

SERVER_FRAMES — полный список кадров сервера, и его ПОРЯДОК является контрактом: список сверяется с вариантами VkServerMsg (plugins/vk/src/ws/protocol.rs) построчно. С Ф5 в нём четвёрка анализа чата — 'analysis_started', 'analysis_delta', 'analysis_done', 'analysis_error' — РОВНО между 'dialog_changed' и 'resync', зеркально enum'у на сервере.

Неизвестный тип кадра не ошибка (сервер может начать присылать новое событие раньше, чем обновится закешированный фронт) — console.debug и всё. Исключение обработчика ловится callHandler и соединение не рвёт. resync и ping разбираются отдельными ветками onFrame.
