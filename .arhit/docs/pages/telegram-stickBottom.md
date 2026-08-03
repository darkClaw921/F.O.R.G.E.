local.stickBottom (tmux-web/static/js/telegram/chat.js) — защёлка «лента прижата к низу». Введена при починке дефекта живой приёмки «при открытии чата он не прокручивается автоматически до самого последнего сообщения» (br forge-ujck). Дефект-близнец VK (br forge-p9dk), причина общая.

ЗАЧЕМ. Высота ленты в момент прокрутки НЕ окончательная: превью вложений грузятся лениво (IntersectionObserver в js/telegram/media.js), и каждое показанное превью растит .tg-media-body с min-height 92px до max-height 184px (THUMB_HEIGHT). Синхронный scrollToBottom() отрабатывает по старой раскладке, а через сотни миллисекунд лента дорастает — низ уезжает под кромку ровно на сумму догруженного. Замер живого стенда (60 сообщений, у каждого четвёртого фото): сразу после renderHistory scrollTop=2911 scrollHeight=3628 clientHeight=717, отступ 0; через 3.0с scrollHeight=3904 при том же scrollTop — отступ 276 = 3 x 92. Дальше дефект самоусиливается: isAtBottom (порог 40px) уже ложно, поэтому ветка stick в onNewMessage не срабатывает больше НИКОГДА (отступ дорос до 453px и не восстановился).

КТО ВЗВОДИТ. scrollToBottom() — он выражает НАМЕРЕНИЕ «показать конец переписки», а не разовое присваивание scrollTop. Плюс resetChat() (пустая лента — это лента у низа: следующий открытый диалог не должен унаследовать «пользователь читал историю»). Начальное значение — true.

КТО СНИМАЕТ. Только обработчик прокрутки в bindScroll(), и только по правилу:

    if (atBottom) local.stickBottom = true;
    else if (top < local.lastScrollTop) local.stickBottom = false;

Обе оговорки обязательны. «От кромки» — иначе дребезг инерционной прокрутки в пару пикселей отменял бы прижатие, стоя при этом внизу. «Уходом вверх» — иначе прижатие снимал бы НАШ СОБСТВЕННЫЙ доскролл: событие scroll доставляется асинхронно, к моменту доставки успевает догрузиться ещё одно превью, и isAtBottom уже ложно, хотя рука пользователя ленты не касалась. Порога в 24px (FOLLOW_CANCEL_UP_PX, отмена следования) здесь нет намеренно: медленная прокрутка вверх идёт шагами по 5–10px и порога не набирает ни разу, а собственные доскроллы идут ВНИЗ и под правило «top < lastScrollTop» не попадают в принципе.

КТО ЧИТАЕТ. onMediaGrew() — единственный потребитель. Виден снаружи через _debugChatState().stickBottom.

ЧЕМ ОТЛИЧАЕТСЯ ОТ VK-ШНОЙ КОПИИ. У Telegram СВОЯ механика следования за ответом бота (js/telegram/follow.js + ResizeObserver в chat.js), у VK — общий слой messenger/follow-core.js через фабрику createFollow. Гард в onMediaGrew поэтому разный по коду (followTarget(Date.now()) против follow.followTarget(Date.now())), хотя и одинаковый по смыслу. Обе копии оставлены сознательно — ровно по той же причине, что и две копии движка комбинаций (см. шапку tests/frontend/messenger_shared_parity.test.mjs): Telegram на общий слой не мигрирован, и правка работающей панели ради красоты графа импортов — риск без выгоды для пользователя.

РАЗВЕДЕНИЕ СО СЛЕДОВАНИЕМ. Пока followTarget(Date.now()) истинно, onMediaGrew() возвращает false и ленту не трогает: у следования своя цель (конец блока строки, а не низ ленты) и свой ResizeObserver. Прирост при этом не теряется — его ловит onFollowResize. Замер в браузере: под живым следованием превью выросли на 276px, лента осталась на scrollTop=2934 (отступ 276); после clearFollow() защёлка немедленно догнала низ (scrollTop 2934 -> 3210, отступ 0).

СВЯЗАННЫЕ: js/telegram/chat.js::scrollToBottom, ::onMediaGrew, ::bindScroll, js/telegram/media.js::setMediaGrowthHandler, js/telegram/format.js::isAtBottom, js/telegram/follow.js::followTarget, страница telegram-scroll-model, страница vk-stickBottom. Регресс: tmux-web/tests/frontend/telegram_scroll_stick.test.mjs (39 проверок, 8 сценариев, 7 мутаций убиты).