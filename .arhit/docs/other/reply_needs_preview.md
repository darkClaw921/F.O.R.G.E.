# reply_needs_preview

plugins/telegram/src/dto.rs

`pub fn reply_needs_preview(reply: &ReplyToDto) -> bool`

Нужно ли ещё достать превью оригинала для этой цитаты.

Ровно `reply.message_id.is_some() && !reply.unavailable && reply.out.is_none()`.

ПОЧЕМУ ПРИЗНАК — ИМЕННО `out`. Его выставляет ТОЛЬКО обогащение (enrich_reply_previews /
enrich_reply_preview), первый приём (reply_to_from_header) не ставит его никогда, даже когда мог бы
угадать. Благодаря этому предикат идемпотентен: повторный проход по уже обогащённой странице не
порождает ни одного сетевого запроса. Проверять по `text` было бы неверно — у сообщения с одним
лишь вложением текста нет и не будет, и такая цитата запрашивалась бы вечно.

Тот же предикат воспроизведён на фронте: reply.js::replyNeedsPreview.

ПОТРЕБИТЕЛИ: reply_ids_to_fetch, enrich_reply_previews, enrich_reply_preview,
dialogs.rs::enrich_single_reply.
