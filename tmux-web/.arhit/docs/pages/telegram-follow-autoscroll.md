# Автопрокрутка ленты Telegram после нажатия кнопки бота

## Проблема
Типовой сценарий inline-кнопок: пользователь жмёт кнопку → бот РЕДАКТИРУЕТ своё же
сообщение (текст длиннее, клавиатура больше) или присылает новое. Строка в ленте
пересобиралась в chat.js::onEdited, но там не было ни замера isAtBottom, ни доскролла —
выросший блок уезжал под нижнюю кромку, и приходилось листать руками.

Обычного stick-to-bottom не хватает: полоса клавиатуры бота над #tg-prompt сама
съедает clientHeight ленты, и isAtBottom перестаёт быть истинным ровно в момент
появления кнопок.

## Решение
Нажатие кнопки включает «следование» за парой (диалог, message_id) на 12 секунд
(FOLLOW_TTL_MS). Пока следование живо, апдейты ЭТОГО сообщения и новые сообщения в
ЭТОМ диалоге тянут ленту к концу блока. Вне следования — прежнее правило isAtBottom.

## Файлы
- static/js/telegram/follow.js — НОВЫЙ модуль, чистая логика без DOM и сети
  (по образцу reply.js): noteFollow / clearFollow / followTarget / isFollowing /
  followsMessage / scrollDecision / scrollTargetForRow / cancelledByScroll /
  scrollBehavior / _debugFollowState. Константы FOLLOW_TTL_MS=12000,
  FOLLOW_SCROLL_PAD=8, FOLLOW_CANCEL_UP_PX=24.
- static/js/telegram/botkb.js — новый колбэк initBotKeyboards({onButtonPress}),
  вызывается в runAction для действий 'callback' и 'send-text' (open-url и
  inline-query ленту не трогают). Регистрация идёт ДО запроса: правка приезжает
  отдельным WS-кадром и может опередить .then.
- static/js/telegram/chat.js — применение решения:
  * замер isAtBottom и followsMessage СТРОГО до мутации DOM (в onEdited — сразу
    после получения row, до replaceWith; в onNewMessage — до вставки строки);
  * afterRowChanged(row, {atBottom, following}) → scrollDecision → follow/stick/none;
  * scrollBlockIntoView(row) через scrollTargetForRow + scrollTo({top, behavior})
    с фолбэком на scrollTop (scrollTo есть не везде);
  * ResizeObserver — ОДИН на вкладку, observe/unobserve на строку; добирает
    отложенный рост (медиа, шрифты, перерисовка клавиатуры);
  * teardownChat() — новый экспорт, зовётся из main.js::teardownTelegram.
- static/js/telegram/main.js — chat.teardownChat() рядом с teardownMedia().

## Снятие наблюдения (утечки недопустимы)
- clearLog() — единственная точка снесения ленты (renderHistory/openDialog/resetChat);
- renderHistory() — ПЕРЕпривязка наблюдателя к пересозданной строке (по образцу
  восстановления подсветки flashId);
- onEdited() — после row.replaceWith(fresh) наблюдение переезжает на новый узел;
- onDeleted() — строка становится заглушкой, следование за ней снимается;
- openDialog() при СМЕНЕ диалога — stopFollow();
- resetChat() — stopFollow() (в т.ч. таймер: живой таймер подвесил бы node-тест);
- teardownChat() — плюс disconnect() самого наблюдателя.

## Что не сломано (и почему)
- Якорь подгрузки истории вверх (loadOlder): scrollBlockIntoView молчит при
  local.loading === true — иначе асинхронный доскролл приземлился бы между замером
  prevHeight/prevTop и восстановлением scrollTop.
- Переход по цитате (jumpToMessage): stopFollow() на входе; плюс scrollTargetForRow
  НИКОГДА не тянет ленту вверх (want <= scrollTop ⇒ null).
- Прилипание к низу и безусловные scrollToBottom в send()/errorLine() не тронуты.
- Отмена ручной прокруткой: bindScroll сравнивает scrollTop с прошлым значением
  (cancelledByScroll), а не выставляет флаг «это я» — присваивание scrollTop
  порождает событие scroll асинхронно. Свои доскроллы идут ВНИЗ и порог не задевают.
- prefers-reduced-motion: window.matchMedia под двойным typeof-гардом
  (globalThis.matchMedia в node-стендах не определён), reduce ⇒ behavior 'auto'.
- Деградация без ResizeObserver: typeof читается В МОМЕНТ ВЫЗОВА; доскролл в момент
  правки работает, теряется только добор отложенного роста.

## Тесты
- tests/frontend/telegram_follow.test.mjs — 58 ассертов, чистая логика без DOM.
- tests/frontend/telegram_scroll_ui.test.mjs — 65 ассертов, реальный chat.js поверх
  фейкового DOM со шпионом ResizeObserver, журналом scrollTo и переключаемым
  prefers-reduced-motion.