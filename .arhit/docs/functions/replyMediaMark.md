# replyMediaMark

tmux-web/static/js/telegram/reply.js

`replyMediaMark(kind) -> '[photo]' | '[video]' | '[document]' | '[sticker]' | '[voice]' | '[audio]' | '[media]'`

Пометка вида вложения для цитаты. Словарь тот же, что у серверного `MediaDto.kind` и у подписи
строки лога (chat.js::messageText): цитата обязана называть вложение так же, как называет его сама
строка оригинала, иначе одно сообщение выглядит как два разных. Неизвестный/пустой kind ⇒ `[media]`.
