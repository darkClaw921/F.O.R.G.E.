# get_messages

plugins/telegram/src/routes/dialogs.rs

`GET /api/telegram/dialogs/:id/messages` — страница истории `{messages: [...], has_more}`.

`before_id` — курсор «строго старше»: Telegram-овский offset_id эксклюзивен, поэтому страницы не
пересекаются и не теряют сообщений. `limit` приводится clamp_limit к [1, MAX_MESSAGES=100].
Пустой диалог — это `{messages: [], has_more: false}`, а не ошибка. Наружу порядок — ВОЗРАСТАНИЕ
id (сортировка, а не reverse: контракт не должен зависеть от порядка чужой реализации).

## Цитаты (изменение фичи ответов)

Каждое сообщение отдаётся вместе с `reply_to` — превью того, на что оно отвечает. Оригиналы,
которых нет на самой странице, добираются ОДНИМ дополнительным батчем (enrich_page_replies →
get_messages_by_id), недостающие помечаются `unavailable: true` (фронт рисует «сообщение удалено»).
Если добирать нечего — сети нет вовсе.

Инвариант «на страницу — не больше одного батча» закреплён компилятором:
`const _: () = assert!(MAX_MESSAGES <= dto::REPLY_FETCH_BATCH);`. Поднимут потолок страницы —
упадёт сборка, а не потеряются цитаты.

Отказ дополнительного запроса НЕ роняет ответ: цитаты остаются «сырыми», фронт дозапросит их
роутом GET /dialogs/:id/messages/:mid.

СВЯЗИ: enrich_page_replies, reply_fetch_plan, clamp_limit, MAX_MESSAGES, get_message.
