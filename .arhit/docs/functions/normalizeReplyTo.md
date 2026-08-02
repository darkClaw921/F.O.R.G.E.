# normalizeReplyTo

tmux-web/static/js/telegram/reply.js

`normalizeReplyTo(msg) -> ?{messageId, sender, senderId, text, mediaKind, quote, out, unavailable}`

Приводит MessageDto к внутреннему описанию цитаты или `null`, если сообщение не ответ.

Поддержаны все три формы, в которых ответ доезжает до фронта:
1. полный `reply_to` с превью — история и ответ на отправку (сервер обогатил батчем);
2. `reply_to` с одним `message_id` — поток апдейтов, где сеть в маппере запрещена;
3. только `reply_to_id` — совместимость: закешированный фронт против сервера без нового поля.

`reply_to_id` НЕ используется, когда пришёл `reply_to`. Это важно для форум-топиков: там Telegram
кладёт id топика в reply_to_msg_id КАЖДОГО сообщения, и старое поле рисовало бы цитату под всей
супергруппой. Серверное `reply_to` эту ловушку уже отсекло (dto::reply_to_from_header).

Все значения нормализуются: пустые строки → null, нечисловой/неположительный id → null.

СВЯЗИ: chat.js::buildQuoteRow (единственный рисующий потребитель), replyNeedsPreview,
mergeReplyFromMessage, replyPreviewText, replyAuthorLabel.
