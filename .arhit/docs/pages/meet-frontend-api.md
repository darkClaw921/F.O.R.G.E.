# tmux-web/static/js/meet/api.js — REST-клиент плагина встреч и ГЕЙТ включённости

(Отдельная страница, а не doc add на 'api.js': в проекте пять файлов с этим именем — core/, telegram/, vk/, bitrix/, meet/ — и запись перетирала бы соседнюю.)

Фронтовая Ф2 плагина forge-meet.

## Что делает

Тонкие обёртки поверх apiFetch (core/api.js) через общий разборщик конверта messenger/http-envelope.js — ровно как в панели VK. Authorization подставляет подменённый window.fetch из core/auth.js, поэтому заголовков в модуле нет вовсе. Как следствие api.js НЕ импортирует core/auth.js и грузится в голом Node — это и позволяет проверять гейт тестом без браузера.

## Таблица-контракт MEET_ROUTES

Девятнадцать записей {method, path, fn, phase}, дословно совпадающих с таблицей в шапке plugins/meet/src/routes/mod.rs. path — ШАБЛОН в записи бэкенда (:id, :name), а не собранный адрес: сравнение должно быть текстовым. Сверка идёт В ОБЕ СТОРОНЫ тестом meet_contract.test.mjs — роут без клиента и клиент без роута валят прогон одинаково. Все пути перечислены сразу, включая те, чьи хендлеры появятся в Ф3-Ф7: список имён и есть договор.

Ф2: getMeetStatus, fetchBadgeRange, connectMeetWs (последняя живёт в js/meet/ws.js — это соединение, а не HTTP-вызов).
Ф3: startMeeting, stopMeeting, cancelMeeting, showMeetingWindow, fetchMeetingScreenshot.
Ф5: getMeeting, getTranscript, getModels, downloadModel, recheckDeps.
Ф6: getSummary.
Ф7: patchSpeakers, fetchMeetingAudio, retryMeeting, getMeetConfig, putMeetConfig.

Двоичные ответы (screenshot PNG, архивный звук) идут через fetchBinary -> blob: у <img> и <audio> нет Bearer-токена, и в remote-mode они просто не загрузились бы. Тот же приём, что у медиа в Telegram/VK/Битриксе.

## Гейт

initMeetGate() спрашивает GET /api/meet/status и мемоизирует ОБЕЩАНИЕ, а не результат: гейт зовут из bootstrap, а в будущем позовут и панели, и порядок между ними не определён — без мемоизации промиса два одновременных вызова дали бы два запроса.

200 -> реестр платформ уезжает в configureMeetPlatforms (источник истины про 'что считать ссылкой на встречу' живёт на сервере: разметку браузер держит в кэше PWA неделями), транспорт батча — в configureMeetStore. С этого момента meetEnabled() истинна.

404 -> плагин запущен без --meet. meetEnabled() НАВСЕГДА false: configureMeetStore не звался, значит стор не умеет ходить в сеть, attachMeetButton выйдет первой строкой, WS не откроется (connectMeetWs сам проверяет гейт), хоткей вернёт false. В консоль не пишется НИЧЕГО — так запускается devforge у всех, кто про встречи не просил.

Вызывается из core/bootstrap.js (там же suspendMeet/resumeMeet на visibilitychange и teardownMeet на beforeunload). Соединение поднимается в гейте, а не при открытии панели: индикатор записи обязан работать, когда человек нажал кнопку в Telegram и ушёл в терминал.

Прочие экспорты: meetStatusSnapshot() — последний ответ /status для панелей; meetGateRequests() — счётчик походов на сервер (тесты и e2e); resetMeetGate() — только для тестов, в браузере этого пути нет.

## Идентичность комнаты фронт не считает

startMeeting отправляет КАНОНИЧЕСКИЙ АДРЕС (поле url из detectMeetUrl) и непрозрачную тройку (plugin, chat_id, message_id). url_key считает бэкенд, разбирая адрес заново. Строки localKey в модуле нет вовсе — это проверяется тестом по исходнику.

## Тесты

tmux-web/tests/frontend/meet_api_gate.test.mjs (88 проверок) и meet_contract.test.mjs (178 проверок).