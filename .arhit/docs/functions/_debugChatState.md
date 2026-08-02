# _debugChatState

_debugChatState() (tmux-web/static/js/telegram/chat.js, экспорт) — снимок состояния ленты Telegram для e2e и node-тестов.

ПОЛЯ: activeDialogId, rendered (размер local.rows), pending (сколько optimistic-строк в полёте), hasMore, loading, messages (длина списка активного диалога), replyToId, replyBarVisible, flashId — и ЧЕТЫРЕ ПОЛЯ СЛЕДОВАНИЯ, добавленные вместе с фичей автопрокрутки:
 * followDialogId: ?string — диалог, за которым следим (из follow.js::_debugFollowState);
 * followMessageId: ?number — сообщение с кнопкой; null означает либо «не следим», либо «следим за диалогом целиком» (кнопка reply-клавиатуры) — различать помогает followActive;
 * followActive: boolean — `!!followTarget(Date.now())`, то есть окно ЕЩЁ живо по часам. Именно это поле, а не followDialogId, отвечает на вопрос «следование работает прямо сейчас»: состояние может быть заполнено, а срок уже истечь;
 * followObserving: boolean — `!!local.followRow`, висит ли наблюдение за размером на какой-то строке.

ЗАЧЕМ ИМЕННО ЧЕТЫРЕ, А НЕ ОДИН ФЛАГ. Срок и адресность следования иначе выводились бы только по побочным эффектам прокрутки («лента дёрнулась / не дёрнулась»), а жизненный цикл наблюдателя — вообще никак; падающий тест в такой формулировке нечитаем. Разделение followActive и followObserving критично: это два независимых механизма (часы и ResizeObserver), и большинство тестов утечки проверяют именно расхождение между ними — «окно истекло, а наблюдатель ещё висит».

ПОТРЕБИТЕЛИ: tmux-web/tests/frontend/telegram_scroll_ui.test.mjs и telegram_scroll_edge.test.mjs (основной способ проверить, включилось/погасло ли следование), e2e/tests/ui/telegram-ui.spec.ts.

СВЯЗАННЫЕ: follow.js::_debugFollowState, follow.js::followTarget, reply.js::_debugReplyState.
