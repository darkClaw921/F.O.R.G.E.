# buildReplyTarget

tmux-web/static/js/telegram/reply.js

`buildReplyTarget(dialogId, msg, dialog?) -> ?{dialogId, messageId, name, preview, out, reply}`

Цель ответа для полосы над полем ввода: всё, что нужно и полосе (имя + превью), и отправке
(messageId), и оптимистичной строке (reply → replyDtoFromTarget).

Возвращает `null`, если у сообщения нет подтверждённого id (optimistic-строка ещё в пути): Telegram
не примет ответ на несуществующий message_id, и кнопка «ответить» у таких строк не рисуется вовсе
(chat.js::buildReplyButton).

`dialogId` сохраняется в самой цели — на нём построен автосброс при переключении чата (replyTarget).
