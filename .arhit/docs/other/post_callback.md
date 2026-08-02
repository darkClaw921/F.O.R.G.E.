# post_callback

plugins/telegram/src/routes/dialogs.rs — обработчик POST /api/telegram/dialogs/:id/messages/:mid/callback
(нажатие inline-кнопки бота, D2).

Контракт:
  body: {"data": string|null (base64 standard+padding), "game": bool}
  200:  {"message": string|null, "alert": bool, "url": string|null, "cache_time": number|null}
Все четыре поля ответа ПРИСУТСТВУЮТ всегда (могут быть null) — undefined и null во фронте не
разъедутся. GET/PUT/DELETE по этому пути дают 405: роут смонтирован ровно на POST.

ПОРЯДОК ПРОВЕРОК — тот же, что у остальных data-роутов, и это не косметика: авторизация (409) →
соединение (503) → разбор пути и тела → резолв диалога → RPC. Если бы разбор шёл раньше проверки
авторизации, неавторизованный клиент мог бы прощупывать существование сообщений по РАЗНИЦЕ
ответов (404 vs 400).

RPC: messages.getBotCallbackAnswer с password: None. Кнопки с requires_password СОЗНАТЕЛЬНО не
поддержаны — Telegram отвечает PASSWORD_MISSING, что маппится в честный 400, а фронт такие кнопки
и так рисует неактивными. Обоснование зафиксировано в rustdoc хендлера.

ПРИВАТНОСТЬ: в лог не уходят ни payload кнопки, ни текст ответа бота.

ОШИБКИ (общий формат {"error":{code,message,retry_after_secs?}}): 409 not_authorized,
503 not_connected, 404 unknown_message (:mid не разбирается / сообщение удалено), 404 unknown_dialog,
400 invalid_input (не-base64, >64 декодированных байт, ни data ни game, DATA_INVALID,
PASSWORD_MISSING), 429 flood_wait + retry_after_secs, 504 bot_timeout, 502 telegram_error.

Валидация без сети вынесена в validate_callback, маппинг ошибок — в map_callback_error.
Фронтовый вызов: api.js::sendCallback, потребитель — botkb.js.
