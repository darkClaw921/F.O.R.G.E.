# bitrix-ws-protocol

plugins/bitrix/src/ws/{mod,protocol}.rs — WS-протокол панели «Битрикс24» и его обработка.

КАДРЫ (tag=type, snake_case; набор заморожен и сверяется с SERVER_FRAMES во фронте тестом server_frame_tags_are_frozen + bitrix_contract.test.mjs): login_state, dialogs_patch, message_new, message_update, message_delete, reaction, typing, read, counters, presence, task_update, upload_progress, download_progress, error, resync, ping.

КТО И КОГДА ШЛЁТ (итог Ф7).
- message_new — роут отправки (своё, мгновенно) + поток событий (чужое, до цикла опроса);
- message_update — роут правки (своё, мгновенно) + ФОЛБЭК-синхронизация (чужое, до минуты: ONIMV2MESSAGEUPDATE живьём не приходит);
- message_delete, reaction — поток событий;
- dialogs_patch, counters, presence — фолбэк-синхронизация;
- resync — Lagged (Р19) и протухший курсор (Р18);
- error — только троттлинг портала (Р16);
- typing, read — НЕ ОТПРАВЛЯЮТСЯ НИКОГДА (§6 плана: входящих событий набора текста и чужого прочтения платформа вебхуку не даёт).

ОБРАБОТКА СОЕДИНЕНИЯ (ws/mod.rs).
- handle_socket: подписка на broadcast ДО отправки снапшота (иначе событие в окне между ними терялось бы навсегда); biased select! с приоритетом shutdown; heartbeat 15с; idle 60с; битый входящий кадр НЕ рвёт соединение.
- broadcast_frame(Result<msg, RecvError>) -> Option<B24ServerMsg> — ОТДЕЛЬНАЯ чистая функция ради тестируемости: Ok → кадр, Lagged → Resync (досылки не бывает: broadcast хранит только хвост), Closed → None (закрываем соединение).
- Фильтрации по активному диалогу НЕТ намеренно: бейдж и тосты обязаны работать с ЛЮБОЙ вкладки devforge.
- ws_heartbeat/ws_idle_timeout — ДАННЫЕ состояния, а не константы: иначе тест idle-таймаута спал бы минуту.

ПОДРОБНОСТИ — страница bitrix-realtime-model.
