# enrich_reply_preview

plugins/telegram/src/dto.rs

`pub fn enrich_reply_preview(message: &mut MessageDto, original: Option<&MessageDto>)`

То же, что enrich_reply_previews, но для ОДНОГО сообщения — там, где батчить нечего: роут
GET /dialogs/:id/messages/:mid и ответ на собственную отправку POST /messages.

`original == None` ⇒ цитата помечается `unavailable: true`.

ЗАЩИТА ОТ ПОДСТАНОВКИ. Оригинал с НЕСОВПАДАЮЩИМ `id` игнорируется (цитата уходит в unavailable, а
не получает чужой текст) — то же правило, что у фронтового mergeReplyFromMessage. Без него ошибка
на вызывающей стороне показала бы пользователю посторонний текст под видом цитаты.

Ничего не делает, если цитаты нет или превью уже собрано (reply_needs_preview == false).

СВЯЗИ: enrich_reply_previews, reply_needs_preview, dialogs.rs::enrich_single_reply,
dialogs.rs::get_message, dialogs.rs::post_message.
