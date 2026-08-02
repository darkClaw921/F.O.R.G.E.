# showReplyBar

tmux-web/static/js/telegram/chat.js (private)

`showReplyBar(target)` — показывает полосу с именем автора и превью цели (textContent).

Placeholder поля ввода НЕ трогается: им владеет полоса клавиатуры бота
(botkb.js::setInputPlaceholder), и подмена затёрла бы подсказку бота.

Полоса строго однострочная (nowrap + ellipsis у обеих подписей в CSS), поэтому съесть поле ввода на
узком экране она не может по построению.
