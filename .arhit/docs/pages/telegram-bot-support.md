# Telegram: поддержка ботов (кнопки и папки)

Дефекты D2 и D3 из жалобы пользователя: «нету поддержки ботов, ни кнопок которые появляются;
при включении папки где есть боты их чаты не отображаются».

## D2 — кнопки ботов

### Backend

- `MessageDto.reply_markup: null | ReplyMarkupDto` — заполняется в `from_grammers_message`
  (через `msg.reply_markup()`); в `from_raw_message` всегда `null` (у `messageService` поля нет
  по схеме).
- `ReplyMarkupDto { kind, rows, resize, single_use, persistent, placeholder }`,
  `ReplyButtonDto { text, kind, data, url, query, requires_password }` — контракт зафиксирован
  дословно, все `Option` сериализуются явным `null`, ни одного `skip_serializing_if`.
- `reply_markup_dto` покрывает все 4 контейнера, `reply_button_dto` — все 18 конструкторов
  кнопок layer 227 и обе формы `switch_inline`. Матч заканчивается `_ => {}` ⇒ kind `"other"`
  с СОХРАНЁННЫМ текстом: bump layer'а не ломает сборку и не теряет кнопку.
- `data` — base64 (standard, с padding) от произвольных БАЙТОВ callback-data. Свои
  `base64_encode` / `base64_decode` (~40 строк): `base64 0.22` уже в `Cargo.lock` от tmux-web,
  выигрыш от крейта нулевой, а плата — лишняя строка в манифесте плагина.
- `POST /api/telegram/dialogs/:id/messages/:mid/callback` — `messages.getBotCallbackAnswer`.
  Валидация без сети — `validate_callback` (msg_id > 0, base64, ≤64 ДЕКОДИРОВАННЫХ байт,
  data|game). Маппинг ошибок — `map_callback_error`, отдельный от `map_invocation`:
  `BOT_RESPONSE_TIMEOUT` приходит с кодом 400, а второй таймаут как -503, поэтому распознавание
  идёт ПО ИМЕНИ и раньше кодовых веток → 504 «бот не ответил»; `MESSAGE_ID_INVALID` здесь значит
  «сообщение удалено» (404), а не «кривой reply_to_id» (400).
- Кнопки с `requires_password` не поддержаны сознательно (`password: None`) — Telegram ответит
  `PASSWORD_MISSING` ⇒ честный 400. В лог не уходят ни payload кнопки, ни текст ответа бота.

### Frontend (`js/telegram/botkb.js`)

Две сущности Telegram разведены по контракту:
- **inline-клавиатура** рисуется ПОД конкретным сообщением и живёт вместе с ним
  (`buildMessageKeyboard`, узел кладётся в `li._tg.kb` в `chat.js::buildMessageRow`);
- **`keyboard` / `hide` / `force_reply`** — это СОСТОЯНИЕ ДИАЛОГА, рисуется полосой над
  `#tg-prompt` (последняя актуальная на диалог). Узел полосы создаётся в JS и вставляется перед
  `#tg-prompt` — `index.html` не тронут.

Поведение кликов: `callback` → POST `.../callback`, кнопка `disabled` на время запроса (дребезг
мыши = три обращения к боту = FLOOD_WAIT); `message`+`alert:false` → тост, `alert:true` → громкий
тост `BOT ALERT` (ttl 15 с), `url` → новая вкладка. `url`/`login_url`/`user_profile` → открытие
только `http`/`https`/`tg`. `switch_inline*` → подстановка query в поле ввода БЕЗ отправки.
`text` → отправка через `chat.js::send`, переданный колбэком в `initBotKeyboards` (свой POST
дублировать нельзя — потерялись бы optimistic-строка, дедуп по `insertMessage` и снятие
«печатает…»). `request_phone`/`request_location`/`request_poll`/`web_view`/`buy`/`game`/`other` и
`callback` с `requires_password` → визуально неактивная кнопка + тост с причиной. `429` →
обратный отсчёт прямо на кнопке БЕЗ автоповтора; `504` → «бот не ответил».

XSS: подпись кнопки — только `textContent`; скобки `[ … ]` рисует CSS через `::before`/`::after`,
поэтому в текстовом узле лежит ровно подпись бота. Из подписи вырезаются управляющие символы и
bidi-override (U+202E «переворачивает» строку — спуфинг «удалить»/«оставить»), ZWJ сохраняется,
обрезка идёт по code points. URL: сначала вычистка управляющих/невидимых символов, ПОТОМ разбор
схемы — иначе `java\0script:` обходит белый список; `//evil.com` и `https:evil` тоже отвергаются.

`chat.js`: `onEdited` пересобирает строку при `row._tg.kb || msg.reply_markup` — это самый частый
сценарий inline-кнопок («нажал → бот отредактировал разметку»), без правки условия дефект выглядел
бы как «кнопки не реагируют». `renderHistory` пересчитывает reply-клавиатуру по ВСЕЙ ленте
(страница вверх приносит старые сообщения — актуальной обязана остаться нижняя), `onNewMessage`
обновляет её даже для неоткрытого диалога, `openDialog` гасит чужую полосу, `resetChat` чистит всё
вместе с таймерами.

CSS: секция `.tg-kb*` — только токены, обе темы работают без отдельных правил. Мобильный блок:
`flex-wrap` + `flex: 1 1 40%` + `min-width: 0` + `text-overflow` — ряд из 4-5 кнопок на 320px не
даёт горизонтальный скролл панели.

## D3 — боты в папках

Две половины дефекта:

1. **Нечем было отличить бота от человека.** В `DialogDto` добавлены `bot` и `contact`
   (`peer_bot_contact` для клиентского пути, поля `RawPeerEntry` для сырого). Фронтовый
   `format.js::dialogCategory(d)` классифицирует диалог в РОВНО ОДНУ категорию папки:
   `group | channel | bot | contact | non_contact | user`. Порядок правил `dialogInFolder`
   дословно по Telegram: `exclude_ids` (безусловно исключает) → `pinned_ids`/`include_ids`
   (безусловно включают) → категория → `exclude_muted`/`exclude_read`/`exclude_archived`.
   Бот попадает под `bots` и ТОЛЬКО под него, даже если он в контактах: в TDLib `is_user_bot`
   проверяется раньше `is_user_contact`. Значение `'user'` — это не категория, а «сервер не
   сказал»: на старом DTO поведение деградирует в прежний OR по `contacts|non_contacts|bots`
   (показать лишнее безопаснее, чем спрятать переписку).
2. **Чат бота мог не попасть в загруженную страницу.** Backend: при ЛЮБОМ явном `?folder=`
   дефолтный limit = `DEFAULT_FOLDER_DIALOGS` = 300 (без параметра остались прежние 100 — первая
   отрисовка после логина не должна замедляться), `folder_dialogs` догружает страницами с ТРЕМЯ
   страховками от вечного цикла (`MAX_FOLDER_PAGES`, `offsets_advanced`, дедуп по `dialog_id`).
   Frontend: `dialogs.js::ensureFullList()` — ЛЕНИВАЯ одноразовая догрузка при первом входе в
   пользовательскую папку (`getDialogs(FOLDER_LIST_LIMIT=300, 'all')` + `mergeDialogs`).
   На старте её не делают сознательно: это несколько RPC на каждом реконнекте WS через
   `refreshAll` — размен одного дефекта на задержку старта и риск FLOOD_WAIT.

Маркер бота в списке: `[BOT]` (`format.js::BOT_MARK` / `botMark`) в `span.tg-dialog-bot` рядом с
существующим `[M]`; текст через `textContent`, отступ даёт текстовый узел-пробел, а не
`margin-left` (иначе метка отъедет вдвое). CSS-класс `.tg-dialog-bot` — `--t-cyan`, приглушается
внутри `.tg-muted`.

## Остаточные ограничения

- Если чат с ботом лежит НИЖЕ 300-го по дате, он не приедет и в расширенной выборке
  (`MAX_DIALOGS` = 500). Радикальное решение — резолвить `include/pinned/exclude_peers` папки
  через `messages.getPeerDialogs` на бэке.
- Saved Messages: TDLib относит диалог с самим собой к `contacts`, а в `DialogDto` признака self
  нет ⇒ свой чат попадёт в `non_contacts`. Лечится полем в DTO.
- `?folder=` — это PEER-папка Telegram (`main|archive|all`), а НЕ id пользовательской
  папки-dialogFilter: пользовательские папки по-прежнему фильтруются на фронте.