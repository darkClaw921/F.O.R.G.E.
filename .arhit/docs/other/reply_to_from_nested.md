# reply_to_from_nested

plugins/vk/src/dto.rs::reply_to_from_nested(m: &NestedMessageRaw, names: &N) -> ReplyToDto — проекция инлайненного reply_message в превью цитаты, БЕЗ единого сетевого вызова.

ГЛАВНЫЙ РАЗМЕН ВСЕГО ПЛАГИНА. У Telegram оригинал цитаты добирается вторым RPC (подсистема обогащения ~250 строк: REPLY_FETCH_BATCH, reply_ids_to_fetch, enrich_reply_previews, reply_originals_index, message_exists). VK инлайнит reply_message ЦЕЛИКОМ в объект сообщения, поэтому вся эта подсистема из VK-версии удалена, а страница истории стоит ОДИН запрос вместо двух.

ЗАПОЛНЯЕТ ReplyToDto: message_id (Option, у инлайненного оригинала может отсутствовать), cmid (conversation_message_id — ИМЕННО он адресует ответ в беседе и служит ключом на фронте), sender/sender_id, text (обрезка truncate_chars по REPLY_PREVIEW_CHARS=140 code points), media_kind (attach::first_kind — у VK текст цитируемого РЕГУЛЯРНО пуст при непустых вложениях, и без этого поля цитата выглядела бы пустой), unavailable (пустой во всех отношениях объект = оригинал удалён или скрыт приватностью).

Вглубь не рекурсирует: у цитаты бывают свои цитаты и пересылки, но в превью нужен один уровень.

ВЫЗЫВАЕТСЯ из message_from_raw (поле reply_to) — там же reply_to_id заполняется из reply_message.id.

ТЕСТЫ: dto::tests::reply_preview_is_built_without_any_extra_request, reply_preview_falls_back_to_the_attachment_kind, reply_preview_is_cut_by_code_points_not_bytes; стенд vk_stub::a_page_of_replies_costs_exactly_one_call (страница из 50 сообщений, 25 из них ответы -> ровно 1 messages.getHistory и 0 messages.getById; фикстура намеренно несёт глобальный id у reply_message, иначе тест не заметил бы возвращения дозапроса).
