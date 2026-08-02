# get_message

plugins/telegram/src/routes/dialogs.rs

`pub async fn get_message(State<Arc<TelegramState>>, Path((String, String))) -> Result<Json<MessageDto>, ApiError>`

Роут `GET /api/telegram/dialogs/:id/messages/:mid` — одно сообщение целиком.

ЗАЧЕМ. Фронту нужен способ показать оригинал цитаты, когда пользователь кликнул по ней, а
оригинала нет в загруженной истории; и способ дотянуть превью для сообщений из потока апдейтов,
где сеть в маппере запрещена.

ОТВЕТ ПЛОСКИЙ — сам MessageDto без обёртки (`{id, dialog_id, …, reply_to}`), в отличие от соседних
`{messages: […]}` и `{message: {…}}`. Так записано в контракте фичи; фронт (chat.js::jumpToMessage
через tgApi.getMessage) потребляет именно так.

ПОРЯДОК ПРОВЕРОК (как у соседей): auth (require_ready) → соединение → разбор `:mid`
(parse_message_id) → резолв peer. Удалённое сообщение приезжает как `messageEmpty`, а не как None,
и отсекается `dto::message_exists` — иначе клиент получил бы пустой DTO вместо честного 404.

Цитата самого сообщения добивается ещё максимум одним запросом (enrich_single_reply); при его
отказе поле остаётся «сырым» (только message_id), но НЕ unavailable.

ОШИБКИ:
- 404 unknown_message — `:mid` не число / ≤ 0, сообщение удалено, не принадлежит диалогу,
  MSG_ID_INVALID | MESSAGE_ID_INVALID | MESSAGE_IDS_EMPTY;
- 404 unknown_dialog — CHANNEL_INVALID | CHANNEL_PRIVATE (приходит с кодом 406!) | PEER_ID_INVALID |
  CHAT_ID_INVALID | USER_ID_INVALID;
- 409 not_authorized — аккаунт не привязан либо RPC 401;
- 429 flood_wait — + retry_after_secs;
- 503 not_connected — нет MTProto либо непонятный отказ Telegram.
Метод только GET (POST/PUT/DELETE ⇒ 405).

РЕГИСТРАЦИЯ. В dialogs::router() между `…/messages` и `…/:mid/callback`; имена параметров во всех
трёх шаблонах одинаковые (`:id`, `:mid`) — при расхождении axum паникует ещё на сборке роутера.

СВЯЗИ: parse_message_id, map_message_error, enrich_single_reply, dto::message_exists,
peers::resolve_peer, api.js::getMessage, chat.js::jumpToMessage.
