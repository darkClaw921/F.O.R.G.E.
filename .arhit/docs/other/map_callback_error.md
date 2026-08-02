# map_callback_error

plugins/telegram/src/routes/dialogs.rs — маппинг ошибок messages.getBotCallbackAnswer в контракт
плагина. ОТДЕЛЬНАЯ функция, а не общий map_invocation.

Сигнатура: map_callback_error(err: &InvocationError) -> ApiError.

ПОЧЕМУ ОТДЕЛЬНАЯ (две ловушки, из-за которых общий маппер дал бы неверный ответ):
1. BOT_RESPONSE_TIMEOUT Telegram шлёт с кодом 400, а второй таймаут приходит как -503 Timeout.
   Распознавание идёт ПО ИМЕНИ и РАНЬШЕ всех кодовых веток — иначе одна и та же беда («бот не
   ответил») уехала бы в «исправьте ввод», а вторая в «переподключитесь». Оба → 504 bot_timeout
   с сообщением «бот не ответил».
2. MESSAGE_ID_INVALID здесь означает НЕ то же, что в POST /messages: там это кривой reply_to_id
   (400 invalid_input), здесь — исчезнувшее сообщение (404 unknown_message). Расхождение
   зафиксировано ОТДЕЛЬНЫМ тестом, чтобы будущее «сведём в один маппер» ломало сборку, а не
   молча портило поведение.

ОСТАЛЬНОЕ: FLOOD_WAIT -> 429 + retry_after_secs; DATA_INVALID / PASSWORD_MISSING -> 400;
прочие RPC-отказы Telegram (500/403) -> 502 telegram_error (не лечится ни вводом, ни реконнектом);
не-RPC ошибки (обрыв, Dropped) -> 503 not_connected (лечится реконнектом).
