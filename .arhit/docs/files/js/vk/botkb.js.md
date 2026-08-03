# js/vk/botkb.js

Клавиатуры сообществ ВКонтакте во фронте (Ф9): инлайн-клавиатура под сообщением и полоса нижней клавиатуры над #vk-prompt.

ДВЕ РАЗНЫЕ СУЩНОСТИ. inline — клавиатура ПОД КОНКРЕТНЫМ сообщением, живёт ровно столько, сколько живёт сообщение. keyboard — СОСТОЯНИЕ БЕСЕДЫ: официальные клиенты держат её над полем ввода, пока сообщество не пришлёт другую, поэтому она рисуется полосой .vk-kb-bar, а не под строкой лога (иначе после десяти сообщений висело бы десять одинаковых клавиатур). У VK нет ни hide, ни force_reply телеграмного контракта: вид клавиатуры это ровно флаг inline, а «клавиатуру убрали» означает «ниже по ленте её больше нет».

ЧЕСТНАЯ ДЕГРАДАЦИЯ — ГЛАВНОЕ В МОДУЛЕ. Нажимаются только text (обычная отправка подписи с payload; отправку делает бэкенд) и open_link (открывает браузер). callback / vkpay / open_app / location РИСУЮТСЯ, но disabled и объясняют причину в title. Прятать их нельзя: пользователь сверяет экран с телефоном, и исчезнувшая кнопка читается как поломка панели, а не как ограничение платформы. Веток applyAnswer / тоста-ответа (телеграмный answerCallbackQuery) здесь НЕТ, кода bot_timeout тоже.

ЭКСПОРТЫ.
  cleanButtonText(raw, max) — вычистка подписи от управляющих и bidi-символов (U+202E переворачивает строку: «отменить» читается как «подтвердить»), обрезка по code points, ZWJ сохраняется ради составных эмодзи.
  normalizeButton(btn) — ReplyButtonDto -> {text, kind, url, color, supported, row, col}; кнопка без подписи выбрасывается. Координаты берутся ИЗ DTO, а не из позиции в массиве, — именно потому, что безымянные кнопки выбрасываются и позиционная нумерация разъехалась бы с серверной.
  normalizeMarkup(markup) — матрица с потолками MAX_ROWS=32 / MAX_ROW_BUTTONS=12 (совпадают с dto.rs); разметка без кнопок = отсутствие разметки.
  buttonAction(btn) — {action: 'press'|'open-url'|'unsupported'}. Флаг supported с бэкенда — условие НЕОБХОДИМОЕ, но не достаточное: вид, который фронт не умеет исполнять, отключается даже при supported:true («нажатие в никуда» хуже честного пояснения).
  latestReplyMarkup(messages) / barTitle(markup) — состояние беседы по загруженной истории.
  initBotKeyboards({onSendText, onButtonPress}) — привязка колбэков (см. ниже).
  buildMessageKeyboard(msg) — узел .vk-kb для строки ленты (зовётся из chat.js::buildMessageRow).
  syncReplyBar / noteReplyMarkup / applyHistoryKeyboard / clearReplyKeyboard / resetKeyboards / _debugBotKbState.

ГРАФ АЦИКЛИЧЕН: модуль НЕ импортирует chat.js. Сети в нём тоже нет: нажатие уходит наружу вызовом onSendText(label, press), где press = {messageId, row, col}. Это chat.js::send — и только так у нажатия остаются optimistic-строка, дедупликация по insertMessage и кнопка [retry]. onButtonPress(dialogId, cmid) включает «следование» ленты (js/messenger/follow-core.js) и зовётся ДО запроса: ответ сообщества приходит отдельным кадром LongPoll и может опередить ответ роута. Ключ — cmid, а не message_id: строки ленты VK индексируются в rowsByCmid.

БЕЗОПАСНОСТЬ. Подпись и ссылка приходят от чужого сообщества: только textContent (скобки [ ] рисует CSS), ссылка — через format.js::safeUrl (белый список http/https/vk ПОСЛЕ вычистки управляющих символов, иначе java\0script: обходит проверку). payload во фронт не попадает вовсе — нажатие адресуется координатами.

Тест: tmux-web/tests/frontend/vk_botkb.test.mjs (122 проверки). Стили: tmux-web/static/css/vk.css (.vk-kb, .vk-kb-bar).
Связанные: vk-button-support, post_button, reply_markup_from_raw, button_is_supported, js/vk/chat.js.
