# tmux-web/static/js/bitrix/api.js

REST-клиент панели Битрикс24 поверх apiFetch и messenger/http-envelope. PATHS — единственное место во фронте, где записаны пути; роуты с сегментами хранятся ШАБЛОНАМИ (':id', ':mid') и подставляются withSeg, чтобы строка дословно совпадала с таблицей plugins/bitrix/src/routes/mod.rs (сверяет bitrix_contract.test.mjs). Функции: getStatus, loginWebhook (ТЕЛО СОДЕРЖИТ СЕКРЕТ, автоповторов нет), logout, getFilters, getDialogs, getMessages (limit/beforeId/afterId — направления взаимоисключающие), getMessage (диалог обязателен: метода «дай сообщение по id» у портала нет). Решения принимаются по err.code, а не по тексту.

ЧТО ДОБАВИЛА Ф6: sendMessage (текст + reply_to/reply_author/reply_text — цитату собирает СЕРВЕР, Р10), editMessage, deleteMessage, likeMessage, markRead, markUnread (message_id обязателен — портал отвергает вызов без него), sendTyping, pinDialog, hideDialog. Новые шаблоны в PATHS: messageLike, dialogRead, dialogUnread, dialogTyping, dialogPin, dialogHide. Автоповторов нет ни у одной функции: отправка не ретраится по контракту (Р9), остальные — потому что решение о повторе принимает пользователь.


Ф9 (отправка файлов): в PATHS добавлен upload: '/api/bitrix/upload' — ходит по
нему НЕ этот модуль, а js/bitrix/upload.js через XMLHttpRequest (у fetch нет
прогресса отправки), но путь записан здесь, чтобы адреса плагина жили в одном
месте и страж контракта сверял их все. sendMessage(dialogId, payload) принимает
payload.uploadId -> тело {upload_id}: сервер зовёт im.v2.File.upload, который
заливает файл И отправляет сообщение одним вызовом, а text становится подписью
и может быть пустым. Идентификатор ОДНОРАЗОВЫЙ (сервер потребляет его до
заливки): повтор с тем же значением даёт 409 invalid_state, а не второй файл.

Ф10 (задачи): getTasks({filter,start,responsible,group}) — страница ФИКСИРОВАНА платформой (50 записей, start=(N-1)*50, параметра limit нет вовсе); filter это ПРЕСЕТ (all|mine|active|expired|closed), а не сырой фильтр портала — у того есть псевдополе STATUS, молча возвращающее ноль записей. getTask(id) — карточка с actions (набор разрешённых ПРЯМО СЕЙЧАС действий, динамичен) и dialog_id (null = задачи без чата, Р20). getTaskCounters() — два независимых семейства одним ответом. taskAction(id, action) — ключ действия берётся ИЗ КАРТОЧКИ, 409 invalid_state означает устаревшую кнопку, а не сбой. PATHS += tasks/tasksCounters/task/taskAction.

## МУЛЬТИПОРТАЛЬНОСТЬ (Ф5/Ф8)

REST-клиент панели «Битрикс24». Тонкие обёртки поверх apiFetch (core/api.js) через общий разборщик конверта js/messenger/http-envelope.js.

ЗАМОРОЖЕННЫЙ КОНТРАКТ ПУТЕЙ: таблица в шапке модуля и объект PATHS обязаны дословно совпадать с таблицей в шапке plugins/bitrix/src/routes/mod.rs — расхождение даёт молчаливый 404, а не ошибку сборки. Сверяет tmux-web/tests/frontend/bitrix_contract.test.mjs.

АДРЕСАЦИЯ ПОРТАЛА (мультипортальность, Ф8). Сервер выбирает портал КАЖДЫЙ запрос: заголовок X-Forge-Portal -> ?portal= -> первый слот реестра. Фронт пользуется заголовком (query-строка оседает в логах прокси). Два способа назвать портал:
1. АКТИВНЫЙ — умолчание; setActivePortal(id) зовёт js/bitrix/portals.js при переключении вкладки, дальше каждый запрос уходит с заголовком сам;
2. ЯВНЫЙ — необязательный параметр portal у обёртки (нужен форме настроек Ф7.5: пикер чатов показывает диалоги ВЫБРАННОГО в форме портала).
Явный портал уже принимают getStatus, logout, getFilters, getDialogs (params.portal), getMessages (opts.portal), getVoiceSettings, setVoiceSettings, fetchBytes и весь CRUD порталов. Любой другой обёртке он добавляется одной строкой: параметр прокидывается третьим аргументом call(path, init, portal).
Экспорт: PORTAL_HEADER ('X-Forge-Portal', совпадает с PORTAL_HEADER в routes/portal.rs), setActivePortal, activePortal, portalHeaders (нужен XHR-загрузке в upload.js — она идёт мимо call).
Заголовок подмешивается в ЕДИНСТВЕННОЙ точке — внутренней call(); rawCall = createCaller(apiFetch) вызывается ровно один раз. Пустой активный портал ⇒ заголовок не ставится вовсе, и сервер штатно отдаёт первый слот.

CRUD ПОРТАЛОВ: getPortals (состав вкладок; порядок массива = порядок вкладок; host НЕ отдаётся, пустой title показывается как id, пустой массив = «порталов ещё нет»), addPortal(webhook, title) (POST /portals; ТЕЛО СОДЕРЖИТ СЕКРЕТ; автоповторов нет ни при каких условиях; неудачная проверка вебхука портал НЕ откатывает), renamePortal(portalId, title) (пустая строка стирает имя), reorderPortals(portalId, order) (ПОЛНЫЙ список id, а не позиция), removePortal(portalId, purgeCache) (?purge_cache=1 — единственный query-параметр панели, у DELETE тела нет).

ТРИ ПРАВИЛА: решения по err.code, а не по тексту; у loginWebhook/addPortal автоповторов НЕТ; модуль импортируется как import * as b24Api.
ИНВАРИАНТ СЕКРЕТА: вебхук проходит через модуль ровно один раз телом POST'а и нигде не задерживается — ни в сторадже, ни в переменной модуля, ни в URL. Разбор трёх форм ввода живёт ТОЛЬКО на бэкенде.
Новый код ошибки: 404 unknown_portal — реакция ровно одна и живёт в portals.js (перечитать состав и переключиться).
