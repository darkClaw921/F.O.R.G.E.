# openExternal

tmux-web/static/js/telegram/botkb.js — открытие внешней ссылки из кнопки бота или из ответа на
нажатие.

Сигнатура: openExternal(rawUrl) -> boolean (открыли ли).

Ссылка проходит safeUrl (белый список http/https/tg после вычистки управляющих символов) и
открывается в новой вкладке с 'noopener,noreferrer'. noopener обязателен: без него открытая
страница получает window.opener и может переписать вкладку панели (tabnabbing), а ссылку дал
чужой бот.
