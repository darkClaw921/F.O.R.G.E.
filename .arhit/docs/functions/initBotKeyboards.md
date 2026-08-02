# initBotKeyboards

tmux-web/static/js/telegram/botkb.js — инициализация модуля клавиатур ботов.

Сигнатура: initBotKeyboards(opts: {onSendText?: (text: string) => void, onButtonPress?: (dialogId: string, messageId: number) => void}) -> void. Оба поля необязательны и принимаются только если это функции; повторный вызов просто перезаписывает колбэки.

onSendText — это chat.js::send, переданный КОЛБЭКОМ. Ключевое решение: свой POST /messages здесь
дал бы дубли в ленте и потерял бы всё, что делает chat.js::send — optimistic-строку, дедуп по
insertMessage при возврате настоящего сообщения по WS и снятие индикатора «печатает…».
Модуль сознательно не знает про HTTP отправки сообщений.

onButtonPress — ДОБАВЛЕН вместе с фичей автопрокрутки ленты (дефект «нажал кнопку бота → сообщение выросло и уехало за экран»). Это chat.js::noteButtonPress: регистрация «следования» за парой (диалог, message_id) на 12 секунд, см. follow.js. Зовётся из runAction ДО отправки запроса и ТОЛЬКО для действий 'callback' и 'send-text' — 'open-url' и 'inline-query' ленту не меняют, и включать для них следование значило бы дёргать прокрутку на постороннем событии. Вызов обёрнут в try/catch: автопрокрутка — удобство поверх нажатия, её падение не должно отменять сам callback боту.

По тому же принципу, что и onSendText: колбэк инжектируется снаружи, поэтому botkb.js не знает ни про #tg-log, ни про ResizeObserver, ни про prefers-reduced-motion — чистая половина модуля продолжает тестироваться отдельно (telegram_botkb.test.mjs), а UI-часть автопрокрутки покрыта telegram_scroll_ui.test.mjs и telegram_scroll_edge.test.mjs.

Полоса reply-клавиатуры создаётся лениво при первом показе и вставляется перед #tg-prompt.

Единственный вызывающий — chat.js::initChat.
