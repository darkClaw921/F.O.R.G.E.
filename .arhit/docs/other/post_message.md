# post_message

plugins/telegram/src/routes/dialogs.rs

`POST /api/telegram/dialogs/:id/messages` — отправка текста либо медиа по `upload_id` с подписью.
Ответ: `{message: MessageDto}` (обёрнутый — в отличие от плоского ответа GET .../messages/:mid).

Принимает `reply_to_id` и превращает его в `input.reply_to(...)` — это и есть серверная половина
ответа на конкретное сообщение. Поле применяется НЕЗАВИСИМО от того, текст это или медиа: на фронте
api.js кладёт replyToId в тело вне веток text/caption/upload_id, поэтому и подпись к файлу, и
обычное сообщение уходят с цитатой. Кривой reply_to_id ⇒ 400 invalid_input (через map_invocation —
именно поэтому для нового роута одного сообщения понадобился отдельный map_message_error).

## Цитата в ответе (изменение фичи ответов)

`message.reply_to` обогащается через enrich_single_reply — максимум один дополнительный RPC, и
только когда реально отправляли ответ. Без этого reconcile во фронте заменял бы optimistic-строку с
готовой цитатой на серверную без неё, и подпись «отвечаю Васе» моргала бы ровно в момент
подтверждения.

СВЯЗИ: enrich_single_reply, dto::enrich_reply_preview, map_invocation, api.js::sendMessage,
chat.js::send, upload.js::runItem.
