# _debugReplyState

tmux-web/static/js/telegram/reply.js

`_debugReplyState() -> {dialogId, messageId, name, preview}` — снимок текущей цели ответа для e2e и
ручной диагностики в консоли. Ничего не мутирует.

Парная ручка в chat.js — `_debugChatState()`, дополненная полями `replyToId`, `replyBarVisible`,
`flashId`: по ней видно, разошлись ли цель (reply.js) и её отображение (полоса в chat.js).
