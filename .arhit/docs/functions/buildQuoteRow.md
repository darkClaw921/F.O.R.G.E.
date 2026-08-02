# buildQuoteRow

tmux-web/static/js/telegram/chat.js (private)

`buildQuoteRow(msg, dialog) -> ?HTMLElement` — строка цитаты над текстом сообщения.

Возвращает null, если сообщение не является ответом (reply.js::normalizeReplyTo).

DOM: `div.tg-reply-quote` > `span.tg-reply-quote-name` + `span.tg-reply-quote-text`. Блок, а не
span: строка сообщения рисуется с `pre-wrap`, и инлайновая цитата слилась бы с текстом. Вставляется
ПЕРВЫМ потомком `.tg-line`.

ЛОКАЛЬНАЯ ДОСБОРКА. Сообщения из потока апдейтов приходят с одним `message_id` (сеть в hot-path'е
воркера запрещена). Если replyNeedsPreview и оригинал уже лежит в ленте — превью собирается
локально через mergeReplyFromMessage: это бесплатно и снимает нужду в запросе за тем, что и так в
памяти вкладки.

КЛИКАБЕЛЬНОСТЬ. `data-reply-to`, `role="button"`, `tabIndex=0` и aria-label ставятся ТОЛЬКО при
известном messageId: у ответа на сторис и у ответа в чужой чат оригинала в этом диалоге нет, и
«перейти» некуда. CSS опирается на тот же признак (`.tg-reply-quote:not([data-reply-to])` не
показывает курсор-палец).

Недоступный оригинал рисуется обычной цитатой с текстом «сообщение недоступно» — пустая цитата
выглядела бы как баг вёрстки. Все тексты — только textContent.
