tmux-web/static/js/vk/chat.js::noteButtonPress(dialogId, cmid) — включает «следование» ленты панели VK за сообщением и берёт его строку под наблюдение ResizeObserver.

ОТДЕЛЬНАЯ СТРАНИЦА, а не запись на элементе: имя noteButtonPress занято одноимённой ВНУТРЕННЕЙ функцией js/telegram/chat.js, и запись на элемент затирала бы её документацию (панели документируются раздельно ровно так же, как раздельно живут их модули).

Экспортирована наружу именно потому, что клавиатуры сообществ (Ф9, js/vk/botkb.js) не имеют права импортировать chat.js — иначе два модуля замкнутся друг на друга; связывание идёт через initBotKeyboards({onButtonPress: noteButtonPress}).

Ключ — cmid (тот же, что у ответов): у VK conversation_message_id нумеруется в каждой беседе с единицы, поэтому follow-core.js сверяет беседу явно. cmid==0 означает «следим за беседой целиком» (кнопка reply-клавиатуры принадлежит чату, а не сообщению).

Наблюдение ставится СРАЗУ, не дожидаясь правки: клавиатура может перерисоваться и без message_edited. Таймер (armFollowTimer) нужен не ради самого срока — его проверяет follow.followTarget по часам — а ради СНЯТИЯ наблюдателя: без него ResizeObserver висел бы на строке до следующей смены беседы.

СВЯЗИ: js/messenger/follow-core.js::createFollow (инстанс панели), afterRowChanged/scrollBlockIntoView в том же chat.js.
ТЕСТЫ: tmux-web/tests/frontend/vk_scroll_ui.test.mjs, vk_scroll_edge.test.mjs.