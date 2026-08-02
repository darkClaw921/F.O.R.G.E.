# enrich_single_reply

plugins/telegram/src/routes/dialogs.rs (private)

`async fn enrich_single_reply(client: &Client, peer: PeerRef, message: &mut MessageDto)`

Добивает цитату ОДНОГО сообщения — максимум один RPC. Отдельно от enrich_page_replies, потому что
батчить тут нечего: оригинал ровно один, и «страницы», из которой его можно было бы взять без
сети, нет.

Ничего не делает, если сообщение не ответ или превью уже собрано (dto::reply_needs_preview).
Из ответа берётся первый существующий (dto::message_exists) элемент, дальше — dto::enrich_reply_preview.

Политика ошибок та же, что у страницы истории: молчим (tracing::debug) и оставляем цитату «сырой».
Пометить `unavailable: true` значило бы показать «сообщение удалено» там, где Telegram просто
попросил подождать.

ДВА ВЫЗЫВАЮЩИХ:
- get_message — новый роут одного сообщения;
- post_message — ответ на собственную отправку. Без этого reconcile во фронте заменял бы
  optimistic-строку с готовой цитатой на серверную без неё, и подпись «отвечаю Васе» моргала бы
  ровно в момент подтверждения. Лишний RPC уходит только когда реально отправляли ответ.

СВЯЗИ: dto::reply_needs_preview, dto::message_exists, dto::enrich_reply_preview, get_message,
post_message.
