# tmux-web/static/js/bitrix/chat.js

Лента диалога панели «Битрикс24» (#b24-log): рендер строк, stick-to-bottom защёлкой, догрузка истории, цикл переписки (отправка/правка/удаление/лайк), режим ввода (ответ/правка), отметка о прочтении и «печатает…».

ДОБАВЛЕНО В forge-92x6 — ПЕРЕХОД К ОРИГИНАЛУ ПО ЦИТАТЕ:
- buildQuoteNode(quote, msg, doc) — кликабельная цитата .b24-line-quote над текстом ответа (button, а не span: фокус, Enter/Space и роль достаются бесплатно, и узел автоматически попадает под REPLY_DBLCLICK_IGNORE);
- buildLine вызывает reply.js::splitQuote и рисует тело БЕЗ строки «> …»;
- jumpToQuote(msgId, quote) — снять защёлку, найти оригинал, при неудаче догрузить до REPLY_JUMP_PAGES=3 страниц, иначе сказать словами;
- revealOrigin / findRow / scrollRowIntoView / flashRow / clearFlash — прокрутка по центру и подсветка .b24-line-flash на REPLY_FLASH_MS=2000 мс;
- local.flashId (подсветка переживает renderHistory), local.jumping (второй клик не запускает второй обход), local.replyById (точная цель ответа своих сообщений этой сессии — при чтении портал REPLY_ID не отражает, Р10);
- _debugChatState() отдаёт flashId и jumping для e2e.

Подробности решения — на странице документации bitrix-reply-jump; чистая логика — в tmux-web/static/js/bitrix/reply.js.
