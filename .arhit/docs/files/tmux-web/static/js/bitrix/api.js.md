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
