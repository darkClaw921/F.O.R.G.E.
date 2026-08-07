# tmux-web/static/js/bitrix/keynav.js

Клавиатурная навигация по панели «Битрикс24» (Ф11): DOM, фокус и состояние курсора. Чистая логика (реестр действий, разбор комбинаций, конфликты) вынесена в keys.js и общий messenger/hotkeys-core.js.

ТРИ ИНВАРИАНТА, РАДИ КОТОРЫХ МОДУЛЬ И СУЩЕСТВУЕТ:
1. Сопоставление по event.code — русская раскладка не должна ломать j/k.
2. b24OwnsKeyboard() УСТУПАЕТ VIM ПО ФОКУСУ: панель владеет клавишами, только когда её пейн видим, оверлей #b24-login закрыт, хоткеи включены настройкой И фокус либо нигде (document.body), либо внутри #bitrix. Кликнув в сайдбар сессий, пользователь возвращает себе vim-навигацию hotkeys.js.
3. preventDefault/stopPropagation зовутся ТОЛЬКО когда runAction(id) вернул true. Ложное «выполнено» съедает клавишу у соседних обработчиков, ничего не сделав, и снаружи выглядит как «иногда не работает».

Слушатель один — document.addEventListener('keydown', onKeyDown, true) в CAPTURE. Молчит при: невидимой панели, выключенных хоткеях, открытом #b24-lightbox (он живёт по своим правилам), наличии .modal-overlay (модалка настроек, в том числе запись новой комбинации). Внутри полей ввода одиночные клавиши не перехватываются вовсе (hotkeys-core::comboAllowedIn), а Escape при активном режиме ответа/правки пропускается дальше — его гасит собственный обработчик chat.js, единственный, кто знает, что именно отменять (chat.js::inputMode()).

КУРСОР хранится ИДЕНТИФИКАТОРОМ, а не ссылкой на узел: колонка и лента перерисовываются целиком при каждой подгрузке. Ключ ленты — message.id (в отличие от VK, где им был cmid: у портала идентификатор сообщения глобальный). Класс b24-kb-cursor.

ЗОНЫ (activeZone, по РЕАЛЬНОМУ фокусу, от вложенного к внешнему): #b24-prompt -> input, #b24-task-card -> task, .b24-kb -> keyboard, .b24-media-card -> media, #b24-log -> log, #b24-contacts -> dialogs. Зона задачи — своя, её нет ни у Telegram, ни у VK.

ДЕЙСТВИЯ НАД СТРОКОЙ (msg.reply/msg.edit/msg.like) выполняются НАЖАТИЕМ КНОПКИ СТРОКИ .b24-line-btn[data-action]: мышь и клавиатура идут одним путём, а «портал не разрешает» выражается отсутствием кнопки. Enter на уже сфокусированной кнопке НЕ дублируется нашим click — иначе порталу ушло бы два действия на одно нажатие, а у задачи это может быть «завершить».

РЕЕСТР ВЛАДЕЛЬЦЕВ КЛАВИАТУРЫ. initB24Keynav() публикует контракт window.ForgeApp.b24Keys = {owns, enabled, invalidate} и регистрирует имя в window.ForgeApp.keyboardOwners — МАССИВЕ, который читает static/hotkeys.js. Реестр введён ровно на этой, третьей панели (до неё имена были захардкожены списком). Массив создаётся ЗДЕСЬ, а не в hotkeys.js: тот исполняется раньше модулей, а js/public-api.js затем заменяет window.ForgeApp целиком.

Настройки читаются ЛЕНИВО: bitrix_hotkeys_enabled по правилу !== false (дефолт ВКЛЮЧЕНО), bitrix_hotkeys — карта переопределений, кэш разбора ключуется СОДЕРЖИМЫМ карты, поэтому сохранённые настройки действуют сразу.

Экспорт: KB_CURSOR_CLASS, hotkeysEnabled, currentBindings, invalidateBindings, panelVisible, b24OwnsKeyboard, clearCursor, activeZone, showHelp, hideHelp, runAction, initB24Keynav, resetB24Keynav, teardownB24Keynav, _debugKeynavState. Тест: tmux-web/tests/frontend/bitrix_keys.test.mjs.

