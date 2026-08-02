# applyHistoryKeyboard

tmux-web/static/js/telegram/botkb.js — пересчёт reply-клавиатуры по ВСЕЙ загруженной ленте.

Сигнатура: applyHistoryKeyboard(messages, dialogId?) -> boolean.

Зовётся из chat.js::renderHistory. Пересчёт именно по всей ленте, а не по приращению: подгрузка
страницы вверх приносит СТАРЫЕ сообщения, и если бы состояние обновлялось «последним увиденным»,
старая клавиатура вытеснила бы актуальную нижнюю. Выбор делает latestReplyMarkup.
