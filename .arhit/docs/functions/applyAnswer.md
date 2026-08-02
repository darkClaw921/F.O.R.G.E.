# applyAnswer

tmux-web/static/js/telegram/botkb.js — применение ответа бота на нажатие callback-кнопки.

Сигнатура: applyAnswer(resp) -> object (результат describeAnswer, для тестов и вызывающего).

Действия по describeAnswer: kind 'toast' → обычный тост с текстом бота; kind 'alert' → «громкий»
тост BOT ALERT с ttl 15 с (Telegram требует, чтобы такое сообщение пользователь заметил);
url → openExternal (белый список схем применяется всё равно); kind 'none' → ничего (бот ответил
молча — нормальный сценарий).

Текст ответа ставится через notify (внутри textContent) — он пришёл от чужого бота.
