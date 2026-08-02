# noteButtonPress

noteButtonPress(dialogId, messageId) (tmux-web/static/js/telegram/chat.js, внутренняя) — точка входа фичи автопрокрутки: «нажали кнопку бота».

ЧТО ДЕЛАЕТ, ТРИ ШАГА:
 1. `noteFollow(dialogId, messageId, Date.now())` — регистрирует/продлевает следование в follow.js. Если вернулось false (пустой dialogId) — выходит немедленно, ничего не трогая;
 2. `armFollowTimer()` — перезаводит таймер истечения (нужен ради снятия ResizeObserver, а не ради самого срока);
 3. `observeFollowRow(local.rows.get(Number(messageId)) || null)` — берёт строку под наблюдение размера ПРЯМО СЕЙЧАС.

ПОЧЕМУ НАБЛЮДЕНИЕ СТАВИТСЯ СРАЗУ, НЕ ДОЖИДАЯСЬ ПРАВКИ. Клавиатура может перерисоваться и БЕЗ события `message_edited` — например, бот дописал ряд в ту же разметку, или картинка в уже существующем сообщении догрузилась. Ждать onEdited означало бы пропустить весь этот класс роста. Если строки в ленте нет (messageId===0 для reply-клавиатуры, либо сообщение ещё не отрисовано), observeFollowRow(null) просто снимает старое наблюдение — следование при этом живёт и работает через onNewMessage.

КТО ЗОВЁТ: botkb.js::runAction через колбэк `onButtonPress`, который инжектируется в initBotKeyboards из chat.js::initChat. Колбэк зовётся ДО отправки запроса боту и только для действий 'callback' и 'send-text'.

ПОЧЕМУ ЭТО ЗДЕСЬ, А НЕ В botkb.js. Тот же принцип, что у onSendText: botkb.js не знает ни про ленту, ни про прокрутку, ни про ResizeObserver — он только сообщает о нажатии. Иначе модуль клавиатур пришлось бы связать с DOM ленты, и его чистая половина перестала бы тестироваться отдельно.

СВЯЗАННЫЕ: follow.js::noteFollow, armFollowTimer, observeFollowRow, stopFollow.
