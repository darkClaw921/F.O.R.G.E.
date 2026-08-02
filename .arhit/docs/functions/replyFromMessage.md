# replyFromMessage

tmux-web/static/js/telegram/reply.js

`replyFromMessage(msg, dialog?) -> ?reply`

Описывает сообщение В КАЧЕСТВЕ ОРИГИНАЛА цитаты (тот же вид, что возвращает normalizeReplyTo).
Нужно там, где оригинал уже есть на руках: выбор цели ответа (buildReplyTarget) и локальная
досборка превью из загруженной ленты (mergeReplyFromMessage).

Забирает из MessageDto: id, автора (имя + id строкой), текст, вид вложения, `out`.
`unavailable` всегда false — сообщение перед нами.
