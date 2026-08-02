# reply_originals_index

plugins/telegram/src/dto.rs

`pub fn reply_originals_index(fetched: &[Option<Message>]) -> HashMap<i32, MessageDto>`

Строит индекс оригиналов для enrich_reply_previews из ответа `get_messages_by_id` (порядок и длина
там совпадают с запросом).

Отсеивает две разновидности «оригинала нет»: дыры (`None`) и `messageEmpty` (см. message_exists).
Именно они и означают «оригинал недоступен», и именно поэтому финальный проход
enrich_reply_previews пометит соответствующие цитаты `unavailable: true`.

Ключ — реальный `m.id()` полученного сообщения.

СВЯЗИ: message_exists, enrich_reply_previews, from_grammers_message,
dialogs.rs::enrich_page_replies.
