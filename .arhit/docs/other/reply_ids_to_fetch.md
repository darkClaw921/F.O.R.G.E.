# reply_ids_to_fetch

plugins/telegram/src/dto.rs

`pub fn reply_ids_to_fetch(messages: &[MessageDto]) -> Vec<i32>`

Id оригиналов, которых на странице НЕТ и которые надо добрать батчем: без повторов, в порядке
первого упоминания. Чистая функция, сети не делает.

ЧТО ОТСЕИВАЕТСЯ:
- сообщения без цитаты и цитаты без message_id (сторис, ответ в чужой чат);
- цитаты, у которых превью уже есть (см. reply_needs_preview);
- оригиналы, лежащие на той же странице — типовой «ответ соседу» не должен стоить ни одного RPC;
- дубликаты (несколько ответов на одно и то же сообщение — один id).

Результат ОБЯЗАН нарезаться вызывающим по REPLY_FETCH_BATCH (100): grammers get_messages_by_id не
чанкует список сам. В роуте истории эту нарезку делает dialogs.rs::reply_fetch_plan, где
недостижимость обрезки сторожит `const _: () = assert!(MAX_MESSAGES <= dto::REPLY_FETCH_BATCH)`.

СВЯЗИ: reply_needs_preview, REPLY_FETCH_BATCH, enrich_reply_previews,
dialogs.rs::reply_fetch_plan / enrich_page_replies.
