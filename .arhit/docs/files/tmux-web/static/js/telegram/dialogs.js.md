# tmux-web/static/js/telegram/dialogs.js

Левая колонка #tg-contacts вкладки Telegram (Фаза 3): DIRECTORIES (~/inbox с amber-счётчиком, ~/archive — заглушка v1) / '// ~/inbox — N NODES' (люди и группы) / CHANNELS.

loadDialogs() -> GET /api/telegram/dialogs -> tgState.dialogs -> renderAll(). Элемент списка двухстрочный: slugName + dialogStatusLine + amber '(N)'. Клик/Enter -> selectDialog -> класс .active (ровно один) + колбэк onSelect, который ставит main.js через initDialogs({onSelect}) — прямой импорт chat.js замкнул бы граф модулей в цикл.

Точечные апдейтеры onOnline/onRead/bumpUnread/setUnread правят ОДИН элемент по сохранённой ссылке li._tg, без перерисовки списка: полная перерисовка сбрасывала бы позицию скролла на каждом апдейте присутствия.

XSS: ни одного innerHTML — только createElement + textContent (title приходит из Telegram; '<img src=x onerror=…>' в имени — рабочий вектор, проверено в браузере: 0 элементов <img>, обработчик не выполняется).
