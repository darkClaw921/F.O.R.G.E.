# vk/chat.js

tmux-web/static/js/vk/chat.js — лента переписки #vk-log, отправка, ответы и «следование» панели VK.

СОСТАВ (Ф6 добавила третий и четвёртый пункты):
1. Лента: renderHistory/openDialog/loadOlder (якорь скролла), optimistic-отправка с reconcile, дедупликация insertMessage, отмена загрузки по поколению (js/vk/ratelimit.js — у VK бюджет 3 rps, десять переключений без отмены это три секунды чужих данных на экране), realtime-обработчики onNewMessage/onEdited/onDeleted/onHistoryCleared/onRead.
2. Пересланные сообщения блоком под строкой (у Telegram аналога нет: там пересылка это плоский заголовок).
3. ОТВЕТЫ. Строки индексируются ДВУМЯ картами: rows (по глобальному message_id — правки и удаления из LongPoll приходят с ним) и rowsByCmid (по conversation_message_id — цитата знает об оригинале только его). buildQuoteRow рисует цитату ПЕРВЫМ потомком строки и ставит data-reply-to=cmid плюс data-reply-mid=message_id (последний нужен ТОЛЬКО для дозапроса оригинала вне страницы: messages.getById понимает лишь глобальный id). buildReplyButton — кнопка «↰» у строк с cmid. bindReplyUi вешает делегированные dblclick/click/keydown на #vk-log и Esc на document (с гардом открытого лайтбокса). startReply/cancelReply/cancelReplyTo + ленивая полоса ensureReplyBar/showReplyBar, встающая insertBefore(#vk-prompt) на КАЖДЫЙ показ (полоса клавиатуры сообщества из Ф9 вставляется туда же и может вклиниться). jumpToMessage(cmid, messageId): строка в ленте -> revealMessage (центрирование + подсветка vk-msg-highlight на REPLY_FLASH_MS, запомненная по cmid и восстанавливаемая renderHistory); иначе догрузка до REPLY_JUMP_PAGES=5 страниц; иначе один запрос getMessage и системная строка replySourceLine, а при неизвестном глобальном id — честное «сообщение #N не найдено».
4. СЛЕДОВАНИЕ: const follow = createFollow() из js/messenger/follow-core.js — СВОЙ инстанс панели (общий дал бы коллизию ключей с Telegram). noteButtonPress(dialogId, cmid) экспортирован для Ф9 (botkb.js не имеет права импортировать chat.js). afterRowChanged применяет решение follow.scrollDecision в onNewMessage и обеих ветках onEdited; отложенный рост добирается одним ResizeObserver на панель; bindScroll гасит следование при прокрутке ВВЕРХ сверх порога.

ОТПРАВКА: send(text, replyToCmid, payload) — undefined значит «взять цель из полосы», null «отправить без ответа». Цель фиксируется ОДИН раз в замыкание, поэтому [retry] уходит в ТО ЖЕ сообщение даже если полосу успели перенацелить; тело POST несёт reply_to_cmid; полоса снимается адресно ТОЛЬКО после подтверждения.

СБРОС ЦЕЛИ: удаление оригинала (onDeleted читает cmid ДО удаления из модели), смена беседы (openDialog), resetChat.

ИНВАРИАНТЫ: всё через textContent (текст и имена — данные из VK); спейсер .vk-log-spacer всегда первый потомок; import * as vkApi против кэша старого модуля.

ТЕСТЫ: vk_chat, vk_reply_ui, vk_scroll_ui, vk_scroll_edge, vk_realtime.
