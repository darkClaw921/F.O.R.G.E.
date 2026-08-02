# replyDtoFromTarget

tmux-web/static/js/telegram/reply.js

`replyDtoFromTarget(target) -> ?{message_id, sender, sender_id, text, media_kind, quote, out, unavailable}`

Собирает `reply_to` В ФОРМАТЕ СЕРВЕРНОГО MessageDto для ОПТИМИСТИЧНОЙ строки своего ответа.

Зачем: цитата обязана появиться сразу вместе со строкой. Если ждать подтверждения сервера, подпись
«отвечаю Васе» мигнёт в момент reconcile, и это читается как сбой. Сервер, в свою очередь, тоже
обогащает `reply_to` в ответе на POST /messages (dialogs.rs::enrich_single_reply) — поэтому замена
оптимистичной строки на серверную проходит без мигания.

`quote` всегда null: фрагмент выделяет отвечающий, а наш UI выделения фрагмента не даёт.
`unavailable` всегда false — мы только что видели оригинал в ленте.
