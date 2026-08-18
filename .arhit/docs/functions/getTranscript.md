# getTranscript

Клиент GET /api/meet/meetings/:id/transcript?offset&limit в static/js/meet/api.js. Ответ — {meeting_id, offset, limit, total, segments}. Пагинация, а не виртуализация: час встречи это 600-1200 реплик, а лента рутинно держит 500+ строк. total считает ВСЕ сегменты встречи, а не размер страницы — по нему попап решает, показывать ли кнопку «ещё». Роут реализован в Ф6 (долг Ф5).
