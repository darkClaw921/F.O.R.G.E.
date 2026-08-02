# revealMessage

tmux-web/static/js/telegram/chat.js (private)

`revealMessage(messageId) -> boolean` — была ли строка в ленте.

Находит строку по id в `local.rows`, прокручивает к ней (scrollRowIntoView) и подсвечивает
(flashRow). Возвращаемое значение — сигнал jumpToMessage «искать дальше не нужно».
