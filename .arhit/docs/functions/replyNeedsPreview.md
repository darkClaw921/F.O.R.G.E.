# replyNeedsPreview

tmux-web/static/js/telegram/reply.js

`replyNeedsPreview(reply) -> boolean` — `messageId !== null && !unavailable && out === null`.

Нужно ли ещё добирать превью оригинала. Признак тот же, что на сервере (dto::reply_needs_preview):
`out` заполняет ТОЛЬКО обогащение, поэтому его отсутствие при известном id и означает «текст и
автор ещё не приехали». Проверять по `text` нельзя — у сообщения с одним лишь вложением текста нет
и не будет, и такая цитата догружалась бы вечно.

Используется chat.js::buildQuoteRow: если превью нет, сначала пробуем собрать его из уже загруженной
ленты (mergeReplyFromMessage), и только клик по цитате уходит в сеть.
