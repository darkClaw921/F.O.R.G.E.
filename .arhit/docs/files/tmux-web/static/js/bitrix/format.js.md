# tmux-web/static/js/bitrix/format.js

ЧИСТЫЕ функции панели Битрикс24: ни DOM-узлов, ни сети, ни состояния — потому модуль целиком грузится в Node и покрыт тестами bitrix_format/bitrix_bbcode. Время (parseDate/formatTime/relativeAge/dayKey/dateSepLabel/timestampLabel), строки и имена (truncate/displayName/dialogKind/kindGlyph/previewText/senderLabel), счётчики и директории (unreadTotal/dirMatches/dirCounts/nodesTitle), список диалогов (findDialog/sortDialogs/mergeDialogs), ЛЕНТА (insertMessage/mergeMessages/oldestId/newestId/isAtBottom/shouldLoadOlder/needsDateSep), вложения (formatBytes/mediaKindLabel), РЕНДЕР BB-СЕГМЕНТОВ УЗЛАМИ (renderSegments принимает документ аргументом ради чистоты; ссылка не становится <a href>, идентификаторы уезжают в dataset, [TIMESTAMP] показывается датой), подписи ошибок (errorLabel).


Ф9 (отправка файлов): группа чистых функций аплоада — UPLOAD_STAGES
(queued|client|portal|sent|failed; средние два дословно совпадают с полем phase
кадра upload_progress), uploadMime(name, type) (браузерный MIME, иначе догадка
по расширению), uploadKind(mime) -> image|video|audio|file (svg и здесь
документ), pastedFileName(mime, now) -> pasted_YYYYMMDD_HHMMSS.ext (браузер
называет все скриншоты image.png), uploadStageLabel(stage, sent, total) — для
фазы portal НЕ СОДЕРЖИТ ЦИФР (Р8: процентов у одношаговой base64-заливки не
существует), uploadQueueLabel(pos, total), uploadRejectReason(size, maxBytes)
(пустой файл и превышение лимита — отказ ДО сети; лимит приходит с сервера).

Ф10 (задачи): TASK_ACTION_LABELS + taskActionLabel(action) — подписи кнопок; словарь закрыт, но НАБОР кнопок приезжает с сервера и незнакомый ключ рисуется как есть (спрятать кнопку, которую портал разрешил, хуже). taskDeadlineLabel(task) — строка срока; ПРОСРОЧКУ решает сервер (у портала такого поля нет, признак вычисляется как deadline<now && status<5), второй экземпляр правила во фронте разъехался бы с первым. taskOfDialog(tasksMap, dialog) — карточка для строки колонки. taskNoChatNote(task) — подпись ветки Р20. taskSummary(task) — сводка одной строкой.
