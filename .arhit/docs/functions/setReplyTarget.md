# setReplyTarget

tmux-web/static/js/telegram/reply.js

`setReplyTarget(target) -> ?target`

Запоминает единственную цель ответа в модульном состоянии. Мусор (нет dialogId или нечисловой
messageId) сбрасывает состояние в null, а не сохраняется.

Состояние ЕДИНСТВЕННОЕ и живёт только здесь: если бы цель хранили и полоса (chat.js), и очередь
загрузок (upload.js), показанная подпись и реально уходящий reply_to_id рано или поздно разъехались
бы. Отсюда — только чтение (replyTarget/replyTargetId); снимает цель chat.cancelReply(), потому что
у полосы две половины — цель здесь и DOM-узел в chat.js.
