# vk-onMediaGrew

onMediaGrew() (tmux-web/static/js/vk/chat.js, экспорт) — «содержимое ленты доросло уже после прокрутки, верни её к низу».

ВЫЗЫВАЕТСЯ ИЗ js/vk/media.js, когда декодировалось превью вложения (событие load на <img> карточки). Подписка ставится в initChat(): setMediaGrowthHandler(onMediaGrew). Направление зависимости одностороннее — media.js о chat.js по-прежнему ничего не знает, он держит анонимную функцию.

ТЕЛО:
    if (!$vkLog || !local.stickBottom || local.loading) return false;
    if (follow.followTarget(Date.now())) return false;
    $vkLog.scrollTop = $vkLog.scrollHeight;
    return true;

ТРИ СЛУЧАЯ, КОГДА ПРИРОСТ ИГНОРИРУЕТСЯ:
 1. лента не прижата к низу (local.stickBottom === false) — пользователь читает историю, и утаскивать его вниз из-за догрузившейся картинки было бы самоуправством;
 2. идёт подгрузка страницы вверх (local.loading) — loadOlder держит СВОЙ якорь между замером scrollHeight и восстановлением scrollTop, и посторонняя прокрутка внутри этого окна необратимо теряет позицию чтения;
 3. живо «следование» за строкой (follow.followTarget) — у него своя цель (конец блока плюс зазор, а не низ ленты) и свой ResizeObserver, и два доскролла спорили бы друг с другом.

ПОЧЕМУ ЭКСПОРТ. Ради регресс-теста и e2e — сама подписка внутренняя. Возвращает boolean «двигали ли ленту», что и проверяет тест.

ПОЧЕМУ НЕ ResizeObserver ПО ВСЕЙ ЛЕНТЕ. Наблюдать пришлось бы каждую строку (рост случается в середине не реже, чем в конце), то есть переставлять наблюдение в восьми точках мутации DOM — renderHistory, systemLine, errorLine, onNewMessage, две ветки send, onEdited, onDeleted. Точечное уведомление из showThumb дешевле и точнее: рост от превью — единственный отложенный, все остальные синхронны с мутацией.

СВЯЗАННЫЕ: vk-stickBottom, js/vk/media.js::showThumb, ::setMediaGrowthHandler. Регресс: tmux-web/tests/frontend/vk_scroll_stick.test.mjs (мутанты «не смотрит на защёлку» и «media.js не сообщает о росте» тест валят).
