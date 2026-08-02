# sendCallback

tmux-web/static/js/telegram/api.js — нажатие inline-кнопки бота.

Сигнатура: sendCallback(dialogId, messageId, opts) -> Promise<{message: ?string, alert: boolean,
url: ?string, cache_time: ?number}>. opts: {data?: ?string, game?: boolean}.
Под капотом POST /api/telegram/dialogs/:id/messages/:mid/callback.

data — НЕПРОЗРАЧНЫЙ base64-токен из ReplyButtonDto.data. Фронт его НЕ декодирует и не толкует:
у Telegram это произвольные байты бота.

НИКОГДА НЕ РЕТРАИТСЯ АВТОМАТИЧЕСКИ. 429 flood_wait при повторе продлевает блокировку аккаунта,
а нажатие кнопки — не идемпотентная операция: бот мог уже выполнить действие и не ответить
вовремя. Повтор — только явным кликом пользователя (кнопка показывает обратный отсчёт).

Ошибки приходят Error'ом с полями code/status/retryAfterSecs — их разбирает
botkb.js::callbackErrorText по КОДУ, а не по тексту.
