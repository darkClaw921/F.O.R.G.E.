# buildMessageKeyboard

tmux-web/static/js/telegram/botkb.js — построение inline-клавиатуры под конкретным сообщением.

Сигнатура: buildMessageKeyboard(msg) -> ?HTMLElement. null для reply/hide/force_reply и для
сообщений без разметки — эти виды рисует полоса над полем ввода, а не строка ленты.

Структура: div.tg-kb → div.tg-kb-row → button.tg-kb-btn. Модификаторы кнопки:
.tg-kb-btn-url (--t-cyan, стрелка ↗), .tg-kb-btn-unsupported (--t-grn-dim, dashed, not-allowed),
.tg-kb-btn-pending (--t-amber, ⋯ — запрос в полёте), .tg-kb-btn-wait (--t-red, обратный отсчёт).

Подпись ставится ТОЛЬКО через textContent; скобки [ … ] дорисовывает CSS через ::before/::after.

Вызывается из chat.js::buildMessageRow после медиа-карточки; узел кладётся в li._tg.kb, поэтому
onEdited умеет понять, что у строки была клавиатура, и пересобрать её.
