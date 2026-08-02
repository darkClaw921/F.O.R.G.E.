# enrich_reply_previews

plugins/telegram/src/dto.rs

`pub fn enrich_reply_previews(messages: &mut [MessageDto], originals: &HashMap<i32, MessageDto>)`

Обогащает цитаты СТРАНИЦЫ истории превью оригиналов. Чистая функция — сети не делает.

АЛГОРИТМ:
1. строит индекс превью (preview_parts) из самой страницы — до мутации, иначе `messages` пришлось
   бы одолжить дважды;
2. поверх кладёт `originals` — оригиналы, добранные батчем по reply_ids_to_fetch (ключ — ЗАПРОШЕННЫЙ
   id; готовый индекс даёт reply_originals_index);
3. проходит по сообщениям и для каждой цитаты, где reply_needs_preview == true, подставляет
   sender / sender_id / text / media_kind / out и снимает unavailable;
4. всё, чего нет ни на странице, ни в originals, помечает `unavailable: true`.

Из-за шага 4 вызывать её следует ПОСЛЕ батча, а не вместо него: вызов до похода в сеть пометил бы
живые оригиналы удалёнными. При пустом плане батча звать с пустой картой — правильный и дешёвый
режим: оригиналы возьмутся прямо со страницы.

`quote` при пометке unavailable НЕ стирается (см. ReplyToDto).

СВЯЗИ: preview_parts/apply_preview (private), reply_needs_preview, reply_ids_to_fetch,
reply_originals_index, dialogs.rs::enrich_page_replies.
