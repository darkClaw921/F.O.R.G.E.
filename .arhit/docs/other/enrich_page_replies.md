# enrich_page_replies

plugins/telegram/src/routes/dialogs.rs (private)

`async fn enrich_page_replies(client: &Client, peer: PeerRef, messages: &mut [MessageDto])`

Добивает цитаты страницы истории превью оригиналов — МАКСИМУМ один RPC на страницу.

ПОРЯДОК:
1. reply_fetch_plan даёт список недостающих id (пусто ⇒ сети НЕТ вовсе: либо цитат нет, либо все
   оригиналы лежат на самой странице; enrich_reply_previews вызывается с пустой картой);
2. один `client.get_messages_by_id(peer, &ids)`;
3. dto::reply_originals_index отсеивает дыры и messageEmpty;
4. dto::enrich_reply_previews подставляет превью и помечает ненайденное `unavailable: true`.

ПОЛИТИКА ОШИБОК. Отказ батча наружу НЕ отдаётся (только tracing::debug): история без превью цитат
полезнее, чем пустой экран с ошибкой. Цитаты остаются «сырыми» (только message_id), и фронт
дотягивает их поштучно через GET /dialogs/:id/messages/:mid — тем же путём, что и для сообщений из
потока апдейтов. Помечать при этом `unavailable: true` НЕЛЬЗЯ: «Telegram притормозил» и «сообщение
удалено» — разные вещи, а пользователь увидел бы вторую.

Вызывается в get_messages ПОСЛЕ сортировки страницы по id.

СВЯЗИ: reply_fetch_plan, dto::reply_originals_index, dto::enrich_reply_previews, get_messages.
