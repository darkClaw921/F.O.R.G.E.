# Модель ответов на сообщения в плагине Telegram

Как `reply_to_id` (голое число) превращается в видимую цитату, что происходит при недоступном
оригинале и как работает переход к оригиналу.

Фича закрывает дословную жалобу пользователя на живом аккаунте:
«все хорошо только нет возможности ответить на выбранное сообщение и также не понятно на какое
сообщение ответил собеседник» — два дефекта:

- **R1** — нельзя ответить на конкретное сообщение из UI. Бэкенд это умел с самого начала
  (POST /dialogs/:id/messages принимает `reply_to_id` → `input.reply_to(...)`), и api.js передавал
  параметр; не хватало ровно фронтового UI.
- **R2** — не видно, на какое сообщение ответил собеседник. В MessageDto был только `reply_to_id`,
  без автора и текста оригинала; цитата не рисовалась вовсе.

## 1. Путь данных: число → цитата

```
Telegram (MessageReplyHeader)
  └─ dto::reply_to_from_header      синхронно, БЕЗ сети: message_id + quote (+ автор/медиа для
     │                              ответа в чужой чат)
     ↓  MessageDto.reply_to = {message_id, quote, out: null, unavailable: false, …}
  ┌──┴─────────────────────────────────────────────────────────────────────────────┐
  │ история GET /messages          POST /messages            поток апдейтов (WS)   │
  │ enrich_page_replies            enrich_single_reply       НИЧЕГО (сеть запрещена)│
  │ ≤ 1 батч на страницу           ≤ 1 RPC, только у ответов                        │
  └──┬─────────────────────────────────────────────────────────────────────────────┘
     ↓  reply_to = {message_id, sender, sender_id, text, media_kind, quote, out, unavailable}
  reply.js::normalizeReplyTo → replyAuthorLabel + replyPreviewText
     ↓
  chat.js::buildQuoteRow → div.tg-reply-quote первым потомком .tg-line
```

Ключ обогащения — предикат `reply_needs_preview` (`message_id.is_some() && !unavailable &&
out.is_none()`). `out` заполняет ТОЛЬКО обогащение, поэтому признак идемпотентен: повторный проход
по уже обогащённой странице не порождает ни одного запроса.

Батч устроен так, чтобы страница истории стоила максимум ОДИН дополнительный RPC:
`reply_ids_to_fetch` дедуплицирует id и выбрасывает оригиналы, лежащие на самой странице (типовой
«ответ соседу» бесплатен), `reply_fetch_plan` режет остаток под лимит `messages.getMessages`, а
`const _: () = assert!(MAX_MESSAGES <= dto::REPLY_FETCH_BATCH);` не даёт этому инварианту тихо
сломаться при подъёме потолка страницы.

Поток апдейтов сознательно оставлен без сети: маппер синхронный и стоит в hot-path'е единственного
воркера на весь аккаунт; сетевой вызов задержал бы все прочие события, а пачка ответов в активном
чате дала бы RPC на сообщение → FLOOD_WAIT на ровном месте. Наружу уходит цитата с одним
`message_id`. Дальше работают два дешёвых механизма: фронт собирает превью из уже загруженной ленты
(`mergeReplyFromMessage`) и, если оригинала в ленте нет, дотягивает его по клику
(GET /dialogs/:id/messages/:mid).

## 2. Три вида цитат, которые нельзя путать

| случай | message_id | что показываем |
|---|---|---|
| обычный ответ в этом диалоге | id оригинала | автор + превью, цитата кликабельна |
| ответ в ЧУЖОЙ чат (`reply_to_peer_id`) | **null** | автор и вид медиа из header'а; перехода нет |
| ответ на сторис (`messageReplyStoryHeader`) | **null** | «сообщение недоступно»; перехода нет |
| форум-топик (`forum_topic && reply_to_top_id == null`) | — | `reply_to == null`, цитаты НЕТ |

Про cross-chat: в нашем диалоге под тем же номером лежит ПОСТОРОННЕЕ сообщение, поэтому отдавать
его id нельзя — переход увёл бы не туда. Про форумы: чтобы отправить сообщение в топик, клиент
кладёт id топика в `reply_to_msg_id` КАЖДОГО сообщения; без этой проверки под всей супергруппой
висела бы цитата «сообщение топика».

Старое поле `reply_to_id` сохранено байт в байт (на него опираются фронт и тесты), поэтому на
форумах и у сервисных сообщений два поля СОЗНАТЕЛЬНО расходятся. Правило разрешения на фронте:
`reply_to` СИЛЬНЕЕ `reply_to_id`.

## 3. Недоступный оригинал

`unavailable: true` означает «показать нечем»: оригинал удалён, недоступен или его не существует
(сторис). Фронт рисует обычную цитату с текстом «сообщение недоступно» — пустая цитата читалась бы
как баг вёрстки.

Две вещи, которые здесь легко сделать неправильно:

1. **`quote` при `unavailable` НЕ стирается.** Процитированный фрагмент приходит вместе с самим
   ответом, а не с оригиналом, и остаётся единственным, что вообще можно показать при удалённом
   оригинале. Так же ведут себя официальные клиенты.
2. **Отказ сети — не «удалено».** Если дополнительный запрос не удался, цитата остаётся «сырой»
   (только `message_id`), и `unavailable` НЕ выставляется: «Telegram притормозил» и «сообщение
   удалено» — разные вещи, а пользователь увидел бы вторую. Основной ответ при этом не падает.
3. **`messageEmpty` — это тоже «удалено».** `messages.getMessages` отдаёт на удалённый id не
   пустоту, а `messageEmpty`, который grammers оборачивает в обычный `Message` с пустым текстом и
   датой-эпохой. Проверка `dto::message_exists` стоит и в индексе оригиналов, и в роуте одного
   сообщения — иначе вместо честного 404 приехал бы пустой DTO.

## 4. Ответ на сообщение (R1): что происходит по шагам

1. Выбор цели: кнопка «↰» на строке (видна при наведении, доступна табом), двойной клик по строке
   (`classifyRowClick` со стоп-листом: медиа-плеер, медиа-карточка, кнопки ботов, сама цитата,
   любые `button`/`a[href]`), либо `startReply` из кода.
2. `buildReplyTarget` собирает цель `{dialogId, messageId, name, preview, out, reply}` и кладёт её
   в ЕДИНСТВЕННОЕ место — `reply.js::state.target`. У строк без подтверждённого id (optimistic ещё
   в пути) цели не будет: Telegram отвергнет ответ на несуществующий message_id.
3. Полоса `.tg-reply-bar` над `#tg-prompt` показывает маркер, имя, превью и ✕; фокус уходит в поле
   ввода. Позиция полосы перепроверяется на каждый показ — лениво создаваемая полоса клавиатуры
   бота вставляется тем же `insertBefore($tgPrompt)` и иначе вклинилась бы между полосой и формой.
4. Отправка. Текст: `chat.js::send` фиксирует цель в замыкание (чтобы `[retry]` ушёл туда же) и
   передаёт `replyToId` в `api.sendMessage`. Медиа: `upload.js::sendFiles` читает
   `replyTargetId(dialogId)` ОДИН раз при постановке в очередь и фиксирует его в item'е — очередь
   последовательная, и к моменту отправки полоса может показывать уже другое сообщение;
   `reply_to_id` получают ВСЕ файлы пачки (в отличие от подписи, которая достаётся первому).
5. Оптимистичная строка получает `reply_to` сразу (`replyDtoFromTarget`) — иначе подпись «отвечаю
   Васе» мигнула бы в момент reconcile. Сервер, в свою очередь, обогащает `reply_to` в ответе на
   POST, поэтому подмена строки проходит незаметно.
6. Сброс цели: успешная отправка (точечно, только если полоса всё ещё указывает на ту же цель), ✕,
   Esc, смена диалога (сверка `dialogId` в `replyTarget` — ответ физически не может уйти в чужой
   чат), удаление целевого сообщения, logout.

## 5. Переход к оригиналу (R2)

Клик (или Enter/Space) по цитате → `chat.js::jumpToMessage`:

1. строка есть в ленте → скролл по центру (`getBoundingClientRect`, а не `offsetTop`: у `#tg-log`
   нет своего `position`) + подсветка `.tg-msg-highlight` на 2000 мс;
2. иначе — до 5 страниц истории вверх, с проверкой активного диалога после каждой подгрузки;
3. иначе — `api.getMessage(dialogId, messageId)` (GET /dialogs/:id/messages/:mid, ПЛОСКИЙ
   MessageDto). Только сервер отличает «оригинал далеко» от «оригинал удалён»; в первом случае
   текст показывается системной строкой `replySourceLine`, во втором — «сообщение недоступно
   (удалено)».

Подсветка хранится ПО ID, а не по узлу, и переприменяется в конце `renderHistory`: подгрузка вверх
перерисовывает ленту целиком и убила бы и ссылку, и класс.

Цитата без `data-reply-to` (сторис, cross-chat) не кликабельна и не показывает курсор-палец — CSS
опирается на тот же признак, что и JS.

## 6. Карта элементов

Backend (plugins/telegram/src):
- `dto.rs` — ReplyToDto, reply_to_from_header, reply_preview_text, reply_needs_preview,
  reply_ids_to_fetch, enrich_reply_previews, enrich_reply_preview, message_exists,
  reply_originals_index, REPLY_PREVIEW_CHARS (140), REPLY_FETCH_BATCH (100).
- `routes/dialogs.rs` — get_message (новый роут), map_message_error (третий маппер ошибок:
  map_invocation уводит MSG_ID_INVALID в 400, а контракт требует 404), parse_message_id,
  reply_fetch_plan, enrich_page_replies, enrich_single_reply.
- `updates.rs` — без сети, обосновано в rustdoc map_update.

Frontend (tmux-web/static):
- `js/telegram/reply.js` — новый ЧИСТЫЙ модуль (без DOM и сети), лист графа импортов.
- `js/telegram/chat.js` — buildQuoteRow, buildReplyButton, startReply/cancelReply, bindReplyUi,
  ensureReplyBar/showReplyBar, jumpToMessage/revealMessage/flashRow, send(text, replyToId).
- `js/telegram/api.js` — getMessage; sendMessage не менялся (replyToId уже уходил и с медиа).
- `js/telegram/upload.js` — replyToId фиксируется в очереди, полоса гасится chat.cancelReply().
- `css/telegram.css` — .tg-reply-quote/-name/-text, .tg-reply-bar/-name/-text, .tg-reply-cancel,
  .tg-reply-btn, .tg-msg-highlight, `.tg-line { position: relative }`.

Тесты: `tmux-web/tests/frontend/telegram_reply.test.mjs`, `telegram_reply_ui.test.mjs`,
`plugins/telegram/src/dto.rs` (unit), `routes/dialogs.rs` (unit),
`tmux-web/tests/telegram_smoke.rs` (инвариант ключей MessageDto).

## 7. Контракт HTTP (кратко)

- `GET /api/telegram/dialogs/:id/messages` — без изменений в форме ответа, но каждое сообщение
  приходит с обогащённым `reply_to`.
- `GET /api/telegram/dialogs/:id/messages/:mid` — **новый**. 200 → ПЛОСКИЙ MessageDto (без обёртки).
  404 unknown_message (`:mid` не число/≤0, удалено, не из этого диалога), 404 unknown_dialog,
  409 not_authorized, 429 flood_wait (+retry_after_secs), 503 not_connected. Порядок проверок:
  auth → соединение → разбор `:mid` → резолв peer.
- `POST /api/telegram/dialogs/:id/messages` — `{message: {…}}` теперь содержит `reply_to` с готовым
  превью; дополнительный RPC уходит только при отправке ответа.