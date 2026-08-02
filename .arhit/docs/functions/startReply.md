# startReply

tmux-web/static/js/telegram/chat.js (export)

`startReply(msg, dialog?) -> boolean`

Выбирает сообщение целью ответа: buildReplyTarget → setReplyTarget (reply.js) → showReplyBar →
фокус в `#tg-input`.

Фокус переносится сразу: выбор ответа — это начало набора, и лишний клик по полю здесь был бы
чистой потерей.

Возвращает false, если нет активного диалога или у сообщения нет подтверждённого id.

ТРИ ТОЧКИ ВЫЗОВА: кнопка «↰» (buildReplyButton), двойной клик по строке (onLogDblClick через
classifyRowClick), и внешние вызовы (e2e). Клавиатурный путь — таб до кнопки + Enter.
