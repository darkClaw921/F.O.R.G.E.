# vk/reply.js

tmux-web/static/js/vk/reply.js — ответы на сообщения панели VK, ЧИСТАЯ логика (без DOM и сети; зависит только от таких же чистых vk/format.js и vk/attachments.js). Копия-адаптация js/telegram/reply.js.

ПОЧЕМУ КОПИЯ, А НЕ ОБЩИЙ МОДУЛЬ. Ключ цели ответа у VK — conversation_message_id (cmid), а не message_id. Это другой ИНВАРИАНТ, а не параметризация: (1) messages.send принимает cmid только внутри forward с is_reply, а reply_to понимает лишь глобальный id; (2) cmid нумеруется В КАЖДОЙ беседе с единицы, поэтому один и тот же cmid в двух беседах — разные сообщения, и сверка беседы обязательна; (3) у оригинала, приехавшего инлайном в reply_message, собственного message.id может не быть вовсе.

СОСТАВ:
- REPLY_PREVIEW_CHARS=140 (совпадает с серверным dto::REPLY_PREVIEW_CHARS), REPLY_GONE_TEXT, REPLY_PENDING_TEXT, REPLY_CLICK_IGNORE (стоп-лист селекторов: .vk-media-player, .vk-media-card, .vk-kb, .vk-reply-quote, button, a[href]).
- truncatePreview(text, max) — обрезка по code points, схлопывание пробелов, многоточие ВХОДИТ в лимит.
- replyMediaMark(kind) — словарь берётся у attachments.js::attachmentKind, поэтому сырые имена типов VK (audio_message, doc) тоже понимаются; неизвестный вид даёт нейтральное [media], а не [unsupported].
- normalizeReplyTo(msg) — три формы: серверный ReplyToDto (reply_to, основной путь), сырой VK reply_message (кадр старой версии из кэша вкладки), голый reply_to_id. Возвращает {cmid, messageId, sender, senderId, text, mediaKind, unavailable}.
- replyNeedsPreview(reply) — у VK превью приезжает вместе с сообщением, поэтому «нужно» = урезанный кадр без текста, вида вложения и автора.
- replyAuthorLabel(reply, dialog, selfId) — своё узнаётся по совпадению senderId с id аккаунта (у VK в reply_message нет флага out); недоступный оригинал даёт ПУСТОЕ имя.
- replyPreviewText(reply) — ГЛАВНОЕ ОТЛИЧИЕ ОТ TELEGRAM: пустой текст при непустых вложениях у VK типовой случай (голосовое, стикер, фото без подписи), поэтому пометка вида идёт и вместо текста, и вместе с ним.
- replyFromMessage / mergeReplyFromMessage (сверка по cmid, при его отсутствии по глобальному id — защита от подстановки чужого текста) / replySourceLine (строка «оригинал вне загруженной страницы», в шапке cmid).
- buildReplyTarget(dialogId, msg, dialog, selfId) -> {dialogId, cmid, messageId, name, preview, out, reply} либо null без cmid (optimistic-строка целью стать не может).
- replyDtoFromTarget(target) — форма ровно серверного ReplyToDto для оптимистичной строки.
- Хранилище цели: setReplyTarget / replyTarget(dialogId) / replyTargetCmid(dialogId) / clearReplyTarget / _debugReplyState — собственный модульный синглтон панели (у Telegram свой), с обязательной сверкой беседы.
- classifyRowClick(target) -> 'reply' | 'ignore'.

ПОТРЕБИТЕЛЬ: js/vk/chat.js. ТЕСТ: tmux-web/tests/frontend/vk_reply.test.mjs (102 проверки); мутация «ключ цели = message_id» краснит разделы 5 и 6.
