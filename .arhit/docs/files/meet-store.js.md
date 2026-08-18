# meet-store.js

Общий стор состояний встреч для трёх лент (Telegram, VK, Битрикс24) и для индикатора записи. Фронтовая Ф2 плагина forge-meet, файл tmux-web/static/js/messenger/meet-store.js.

РОЛЬ: шина между транспортом и панелями. WS-клиент js/meet/ws.js разбирает кадр и зовёт метод стора; панели подписаны на стор через subscribeMeet. Ни одного импорта 'плагин -> панель' и ни одной записи в window.ForgeApp: панели импортируют стор ровно в ту же сторону, в какую уже импортируют messenger/analysis-core.js. Сам модуль НЕ ИМПОРТИРУЕТ НИЧЕГО — он нижний слой (проверено тестом по исходнику).

СИНХРОННОЕ ЧТЕНИЕ. getMeetState(key) отвечает синхронно и никогда не возвращает Promise, потому что её зовут прямо в билдере строки ленты. Прогретый кэш отдаёт ТОТ ЖЕ замороженный объект — глиф кнопки физически не может мигнуть при перерисовке. Промах ставит ключ в очередь и возвращает null. peekMeetState читает без побочного эффекта постановки в очередь.

БАТЧ. Промах кладёт ключ в очередь; через BATCH_DELAY_MS=40 уходит ОДИН запрос GET /api/meet/badges диапазоном min_id..max_id на каждую пару (плагин, чат). Ключи в полёте не дублируются (набор inflight). Разброс id шире MAX_BADGE_RANGE=1000 режется функцией chunkRanges на отрезки — иначе сервер ответил бы 400, а выглядело бы это как 'кнопки не появились'. Сбой транспорта НЕ кэшируется: следующая перерисовка попробует снова, но сам стор по таймеру не ходит.

ПОЛЛИНГА НЕТ. TERMINAL_STATUSES (ready/failed/cancelled, дословно MeetStatus::is_terminal) не перезапрашиваются никогда и переживают resync. Нетерминальные обновляются только кадрами WS. Единственный дозапрос — resyncMeet(): сбрасывает нетерминальные записи и добирает их одним батчем на чат.

КЛЮЧ. meetKey(plugin, chatId, messageId) -> 'plugin|chat|id' (SourceKey бэкенда). Ссылка на встречу в ключ НЕ входит: идентичность комнаты (url_key) считает только бэкенд.

КАДР meeting_update НЕ НЕСЁТ ТРОЙКУ-АДРЕС. MeetingRow — строка таблицы meetings, источников в ней нет. Поэтому здесь два индекса: cache (строка ленты -> состояние) и byMeeting (meeting_id -> множество строк). Связь узнаётся из ответа батча (BadgeEntry несёт и meeting_id, и chat_id/message_id) либо из ответа POST /api/meet/meetings через linkMeeting(). Кадр о встрече, которой не соответствует ни одна известная строка, не теряется: он попадает в реестр meetings, и индикатор показывает её с любой вкладки.

ЕДИНИЦЫ ВРЕМЕНИ. На проводе они РАЗНЫЕ: started_at/ended_at — unix-СЕКУНДЫ (chrono::Utc::now().timestamp()), duration_ms — миллисекунды. Перевод делается ОДИН раз на границе (secondsToMs), и единица зашита в имя поля состояния: startedAtMs/endedAtMs. elapsedMs(startedAtMs, now) считает ВЫЧИТАНИЕМ, а не инкрементом.

ЖИВОЙ ТАЙМЕР. Один setInterval на документ: заводится при появлении первой активной встречи (ACTIVE_STATUSES = MeetStatus::ACTIVE), гасится при исчезновении последней, тикает раз в секунду событием {type:'tick'}. stopMeetTicker() гасит его на выгрузке страницы — интервал переживает bfcache и стрелял бы по мёртвому DOM.

ГЕЙТ. configureMeetStore({fetchStates, batchDelayMs, timers}) зовётся ровно из одного места — initMeetGate() в js/meet/api.js, и только при 200 от /api/meet/status. meetEnabled() истинна тогда и только тогда, когда транспорт подставлен. Без него getMeetState отдаёт null и НИЧЕГО не ставит в очередь.

API кадров (по одному методу на вариант MeetServerMsg): applyMeetSnapshot, applyMeetingUpdate, applyMeetDeps, applyMeetProgress, applyMeetTranscriptDelta, applyMeetParticipants, applyMeetNeedsHuman, applyMeetError, resyncMeet, noteMeetConnection. Чтение: getMeetState, peekMeetState, getMeeting, getMeetProgress, getMeetDeps, getMeetLimits, liveMeetings, elapsedMs. Служебное: meetKey, parseMeetKey, chunkRanges, flushMeetQueue, subscribeMeet, linkMeeting, applyBadges, resetMeetStore, stopMeetTicker, _debugMeetStore.

Реестр meetings ограничен MEET_REGISTRY_CAP=500 записей; вытесняются только завершённые и не привязанные ни к одной строке.

Тесты: tmux-web/tests/frontend/messenger_meet_store.test.mjs (128 проверок).
