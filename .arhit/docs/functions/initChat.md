# initChat

initChat(opts) (tmux-web/static/js/telegram/chat.js, экспорт) — единственная точка инициализации ленты Telegram: привязка внешних колбэков и всех обработчиков.

ПАРАМЕТР opts:
 * onRead(dialogId, maxId) — вызвать POST /read. Сам запрос делает main.js, чтобы chat.js не зависел от dialogs.js и от порядка импортов;
 * presenceOf(dialogId) — присутствие собеседника для шапки сессии (живёт в dialogs.js по той же причине).

ЧТО ДЕЛАЕТ, В СТРОГОМ ПОРЯДКЕ: bindScroll() → bindPrompt() → bindReplyUi() → initMedia() → initBotKeyboards({...}).

ПОРЯДОК bindReplyUi ПОСЛЕ bindPrompt ОБЯЗАТЕЛЕН: у полосы ответа свой флаг replyBound, а общий local.bound проверяет ещё и bindScroll — вклиниваться в него означало бы молча отключить подгрузку истории.

ДВА КОЛБЭКА В initBotKeyboards:
 * onSendText: (text) => send(text) — кнопки reply-клавиатуры отправляют обычный текст. Передаётся именно chat.js::send, а не свой POST в botkb.js: иначе у таких сообщений не было бы optimistic-строки, дедупликации через insertMessage и снятия «печатает…», и в ленте появлялись бы дубли;
 * onButtonPress: (dialogId, messageId) => noteButtonPress(dialogId, messageId) — ДОБАВЛЕН вместе с фичей автопрокрутки. Нажатие кнопки включает «следование»: результат нажатия обязан доехать до глаз пользователя сам (см. follow.js). Инжектируется ровно по образцу onSendText, поэтому botkb.js по-прежнему ничего не знает ни про ленту, ни про прокрутку.

ИДЕМПОТЕНТНОСТЬ: bindScroll/bindPrompt защищены local.bound, bindReplyUi — replyBound, initMedia — своим флагом, initBotKeyboards перезаписывает колбэки. Повторный вызов безопасен.

СВЯЗАННЫЕ: noteButtonPress, bindScroll, botkb.js::initBotKeyboards, main.js (единственный вызывающий).
