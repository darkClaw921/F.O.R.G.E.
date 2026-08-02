# tmux-web/static/js/telegram/keynav.js

Клавиатурная навигация по панели Telegram — DOM-слой фичи 'взаимодействие только через клавиатуру' (forge-nert). Чистая логика (реестр действий, разбор комбинаций, конфликты) вынесена в keys.js; здесь только дерево, фокус и состояние курсора.

ЗАДАЧА: пройти путь 'выбрать чат → прочитать → ответить → отправить → нажать кнопку бота' вообще не трогая мышь.

КАК УЖИВАЕТСЯ С ОСТАЛЬНЫМИ ОБРАБОТЧИКАМИ.
1) static/hotkeys.js (vim-навигация по сессиям) занимает j/k/h/l/g/1/2/3/? глобально. Пока открыта панель Telegram, он уступает — гейт telegramOwnsKeyboard вызывается им через window.ForgeApp.telegramKeys.owns(). Уступает НЕ 'всегда, когда плагин включён', а только когда фокус реально внутри панели (или нигде): кликнув в сайдбар сессий, пользователь снова получает vim.
2) Слушатель ставится в capture на document, но НЕ глушит события вслепую: preventDefault/stopPropagation зовутся только когда действие реально что-то сделало (runAction вернул true). Ложное true съело бы клавишу у соседей, ничего не выполнив.
3) Внутри полей ввода одиночные клавиши не перехватываются вовсе (keys.js::comboAllowedIn).

КУРСОР хранится не ссылкой на узел, а идентификатором (dialogId / message_id): и колонка чатов, и лента перерисовываются целиком при каждой подгрузке, и ссылка на узел протухала бы после любого обновления. Класс tg-kb-cursor навешивается на найденный по id узел при каждом перемещении; строки ленты фокусируются программно (tabIndex=-1 + focus с preventScroll), чтобы работали скринридер и последующий Enter.

ЗОНЫ. activeZone() считается по РЕАЛЬНОМУ document.activeElement ('input' | 'keyboard' | 'log' | 'dialogs'), а не по сохранённому полю: мышь и Tab двигают фокус мимо нас, и врать о положении курсора после этого нельзя.

ДЕЙСТВИЯ (runAction): focus.dialogs/log/input, nav.next/prev (контекстные — чаты, лента или ряд кнопок бота), nav.activate (Enter: открыть чат / нажать сфокусированную кнопку / открыть медиа / перейти по цитате), dialog.unread (по кругу), dir.next/prev, msg.reply (chat.js::startReply), msg.quote, msg.copy (navigator.clipboard + тост), msg.buttons, log.bottom, log.older (прокрутка вверх — подгрузку делает штатный chat.js::bindScroll, дублировать loadOlder значило бы завести вторую точку правды о пагинации), ui.attach, ui.theme, ui.help, ui.escape.

ГЕЙТЫ onKeyDown: панель скрыта, оверлей логина открыт, настройка выключена, открыт лайтбокс (там свои стрелки и Esc), открыта модалка настроек (.modal-overlay — там записывают новые комбинации, и 'j' не должна дёргать навигацию).

НАСТРОЙКИ читаются лениво в момент нажатия через window.ForgeApp.state.userSettings — ровно как в hotkeys.js: модуль грузится раньше ответа /api/user-settings, и снимок, взятый на старте, застыл бы навсегда. hotkeysEnabled() сравнивает !== false, а НЕ === true: дефолт этой настройки ВКЛЮЧЕНО, и отсутствие поля обязано означать 'работают'. Кэш разбора привязок ключуется по СОДЕРЖИМОМУ карты переопределений, поэтому сохранённые настройки действуют сразу, без перезагрузки страницы.

ЖИЗНЕННЫЙ ЦИКЛ: initKeynav({onNotice}) из main.js::initTelegram (идемпотентен), resetKeynav при смене диалога (курсор прошлого чата недействителен — в новой ленте тот же message_id принадлежит другому сообщению), teardownKeynav из main.js::teardownTelegram (снимает document-слушатель и оверлей шпаргалки, иначе они переживут страницу вместе со ссылками на её узлы).

Экспорты: KB_CURSOR_CLASS, hotkeysEnabled, currentBindings, invalidateBindings, panelVisible, telegramOwnsKeyboard, clearCursor, activeZone, showHelp, hideHelp, runAction, initKeynav, resetKeynav, teardownKeynav, _debugKeynavState. Тесты: tmux-web/tests/frontend/telegram_keynav.test.mjs (103 ассерта).
