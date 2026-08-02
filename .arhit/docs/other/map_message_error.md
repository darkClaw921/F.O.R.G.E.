# map_message_error

plugins/telegram/src/routes/dialogs.rs

`pub fn map_message_error(err: &InvocationError) -> ApiError`

Третий маппер ошибок плагина — для `messages.getMessages` / `channels.getMessages`.

ЗАЧЕМ ОТДЕЛЬНЫЙ. map_invocation уводит MSG_ID_INVALID / MESSAGE_ID_INVALID в `400 invalid_input`,
а контракт роута get_message требует `404 unknown_message`. Править сам map_invocation нельзя — на
нём держится поведение «кривой reply_to_id в POST /messages ⇒ 400».

ПОРЯДОК ВЕТОК ЗНАЧИМ:
1. 420 ⇒ 429 flood_wait + retry_after_secs;
2. код 401 ⇒ 409 not_authorized;
3. MSG_ID_INVALID | MESSAGE_ID_INVALID | MESSAGE_IDS_EMPTY ⇒ 404 unknown_message;
4. имя содержит PEER_ID_INVALID | CHANNEL_INVALID | CHANNEL_PRIVATE | CHAT_ID_INVALID |
   USER_ID_INVALID ⇒ 404 unknown_dialog. CHANNEL_PRIVATE Telegram присылает с кодом 406, поэтому
   сверка идёт ПО ИМЕНИ, а не по коду;
5. всё прочее (включая не-RPC ошибки) ⇒ 503 not_connected. Контракт роута знает ровно четыре
   исхода, и «непонятный отказ» честнее показать как «попробуйте позже», чем как «исправьте ввод»:
   исправлять клиенту нечего — в запросе нет ничего, кроме пути.

СВЯЗИ: map_invocation, map_callback_error, ApiError, get_message.
