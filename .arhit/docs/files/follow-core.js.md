# follow-core.js

tmux-web/static/js/messenger/follow-core.js — общий слой мессенджеров: «следование» ленты за растущим сообщением, ЧИСТАЯ логика (без DOM, без сети, часы инжектируются параметром nowMs).

ЗАЧЕМ. Нажатие кнопки бота/сообщества — явный интент: бот редактирует своё сообщение (текст длиннее, клавиатура на два ряда больше) или присылает новое, строка растёт и уезжает под нижнюю кромку. Обычного stick-to-bottom не хватает: пользователь почти всегда уже отлистан вверх (полоса клавиатуры съедает высоту ленты), а рост бывает отложенным (догрузка картинки, перерисовка). Поэтому нажатие включает следование за парой (диалог, сообщение) на 12 секунд.

ГЛАВНОЕ АРХИТЕКТУРНОЕ РЕШЕНИЕ — ФАБРИКА createFollow({ttlMs, scrollPad, cancelUpPx}), а не модульный синглтон (как в telegram/follow.js). Общий инстанс на две панели дал бы КОЛЛИЗИЮ КЛЮЧЕЙ: peer_id беседы VK 2000000001 и dialog_id супергруппы Telegram 2000000001 — разные чаты, а состояние сравнивает их строкой; нажатие в одной панели тянуло бы ленту другой. Каждая панель делает свой const follow = createFollow() в своём chat.js.

ВОЗВРАЩАЕТ инстанс: noteFollow(dialogId, messageId, nowMs, ttlMs?) — включить/продлить/перецелить (пустой dialogId -> false, прежняя цель не затирается); clearFollow() -> была ли цель; followTarget(nowMs) -> {dialogId, messageId} | null (часы, прыгнувшие назад, следование не рвут: now < untilMs это «ещё не истекло»); isFollowing(dialogId, nowMs); followsMessage(dialogId, messageId, nowMs) — диалог сверяется ЯВНО, messageId==0 значит «диалог целиком» (кнопка reply-клавиатуры); scrollDecision({atBottom, following}) -> 'follow'|'stick'|'none' (только строгие true); scrollTargetForRow(log, row, pad) -> новый scrollTop или null, ВВЕРХ не тянет никогда (иначе перебивал бы переход по цитате), зажат потолком scrollHeight-clientHeight; cancelledByScroll(prevTop, nextTop, threshold) — решение по НАПРАВЛЕНИЮ, а не по флагу «это был я» (присваивание scrollTop порождает scroll асинхронно); scrollBehavior(reduceMotion); _debug().

КОНСТАНТЫ: FOLLOW_TTL_MS=12000, FOLLOW_SCROLL_PAD=8, FOLLOW_CANCEL_UP_PX=24 (порог, а не любое уменьшение: инерционная прокрутка даёт отскок в единицы пикселей).

ПОТРЕБИТЕЛИ: js/vk/chat.js (const follow = createFollow()). Telegram НЕ мигрирован намеренно — его js/telegram/follow.js остаётся вместе со своими тестами; миграция это отдельная задача после стабилизации VK.

ТЕСТ: tmux-web/tests/frontend/messenger_follow_factory.test.mjs (94 проверки). Мутация «вернуть модульный синглтон» краснит раздел независимости инстансов.
