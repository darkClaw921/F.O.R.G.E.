# getMessage

tmux-web/static/js/telegram/api.js

`getMessage(dialogId, messageId) -> Promise<object>`

Обёртка над `GET /api/telegram/dialogs/:id/messages/:mid` через общий `call()`. Возвращает ПЛОСКИЙ
MessageDto (не `{message: …}`) — так записано в контракте роута; chat.js::jumpToMessage потребляет
его именно так (`dto.id`, `replySourceLine(dto)`).

Зачем нужен: оригинал цитаты может лежать далеко за пределами загруженной истории. Догонять его
через `getMessages` значило бы качать страницы вслепую; роут отвечает за один id и честно отличает
«далеко» от «удалено».

ОШИБКИ — Error с полями .status/.code/.retryAfterSecs: `unknown_message` | `unknown_dialog` (404),
`not_authorized` (409), `flood_wait` (429, + retryAfterSecs), `not_connected` (503).
АВТОПОВТОРА НЕТ: решение о повторе принимает вызывающий по КОДУ, а не по тексту.
