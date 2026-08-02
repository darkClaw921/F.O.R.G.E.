# botMark

tmux-web/static/js/telegram/format.js — метка бота для строки списка диалогов (D3).

BOT_MARK = '[BOT]' — константа в одном терминальном алфавите с MUTED_MARK ('[M]').
botMark(bot) -> string: '[BOT]' либо ''.

Рисуется в dialogs.js::buildRow рядом с '[M]' в span.tg-dialog-bot; текст ставится через
textContent (XSS-инвариант файла), узел кладётся в li._tg.bot.

ОТСТУП даёт ТЕКСТОВЫЙ УЗЕЛ-ПРОБЕЛ, а не CSS margin-left (тот же приём уже используется в строке
статуса). Поэтому в .tg-dialog-bot нужны только color: var(--t-cyan) и font-size: 10px —
добавленный margin-left отодвинул бы метку вдвое.

Точечного патча метки из WS нет сознательно: флаг bot в рантайме не меняется, а local.rows
очищается на каждом renderAll — любой «патч после рендера» всё равно терялся бы.
