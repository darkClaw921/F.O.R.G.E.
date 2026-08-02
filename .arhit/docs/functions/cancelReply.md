# cancelReply

tmux-web/static/js/telegram/chat.js (export)

`cancelReply() -> boolean` — была ли цель.

Снимает выбранный ответ ЦЕЛИКОМ: и цель в reply.js (clearReplyTarget), и полосу над полем ввода
(`local.replyBar.hidden = true`). Именно эту функцию, а не reply.clearReplyTarget, обязаны звать
внешние модули (upload.js после успешной постановки файлов в очередь): у полосы две половины — цель
в reply.js и DOM-узел здесь, — и сброс половины оставил бы полосу висеть над пустым состоянием.

Точки вызова: кнопка ✕ полосы, Esc (onReplyEscape), успешная отправка (send → cancelReplyTo),
удаление целевого сообщения (onDeleted → cancelReplyTo), смена диалога (openDialog), logout,
upload.js::sendFiles.
