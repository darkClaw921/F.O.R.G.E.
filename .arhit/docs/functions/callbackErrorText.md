# callbackErrorText

tmux-web/static/js/telegram/botkb.js — человеческий текст ошибки нажатия кнопки.

Сигнатура: callbackErrorText(err) -> string.

РЕШЕНИЕ ПРИНИМАЕТСЯ ПО КОДУ КОНТРАКТА, А НЕ ПО ТЕКСТУ сообщения: тексты меняются, коды — это
контракт (routes/error.rs). Соответствие:
- flood_wait / 429 → «слишком часто — повтор через Ns» (секунды из callbackRetrySecs);
- bot_timeout / 504 → «бот не ответил» (это не поломка панели, а «бот думает долго», повтор
  осмыслен);
- unknown_message → «сообщение недоступно — кнопки устарели»;
- unknown_dialog → «диалог недоступен»;
- not_authorized → «нет авторизации в Telegram»;
- not_connected → «нет соединения с Telegram»;
- invalid_input → «Telegram отклонил нажатие»;
- telegram_error → «Telegram отклонил запрос»;
- иначе err.message либо «не удалось нажать кнопку».
