# _debugFollowState

_debugFollowState() -> {dialogId: ?string, messageId: ?number, untilMs: number} (tmux-web/static/js/telegram/follow.js) — снимок состояния «следования» для e2e и диагностики.

ВОЗВРАТ: dialogId и messageId нормализованы к null, когда пусты (`state.dialogId || null`), untilMs отдаётся как есть — по нему видно и «следования нет» (0), и точный момент истечения. Функция ЧИСТАЯ: срок годности она НЕ проверяет, живость надо спрашивать у followTarget(Date.now()).

ЗАЧЕМ. Без снимка адресность и срок следования выводились бы только по побочным эффектам прокрутки — «лента дёрнулась/не дёрнулась», — что делает падающий тест нечитаемым. Тот же приём и то же соглашение об именовании, что у reply.js::_debugReplyState и chat.js::_debugChatState (префикс `_` = не публичный контракт UI, а точка наблюдения).

ГДЕ ИСПОЛЬЗУЕТСЯ: chat.js::_debugChatState разворачивает его в поля followDialogId / followMessageId и дополняет собственными followActive (`!!followTarget(Date.now())`) и followObserving (`!!local.followRow`); tests/frontend/telegram_follow.test.mjs секция «Снимок для диагностики»; telegram_scroll_ui.test.mjs и telegram_scroll_edge.test.mjs — как основной способ проверить, что следование включилось/погасло.
