# Модель прокрутки ленты Telegram (#tg-log)

Единый обзор ВСЕХ источников, которые двигают ленту переписки, и правил, по которым они друг другу не мешают. Написано после исправления дефекта «нажал кнопку бота — сообщение выросло и уехало за нижнюю кромку, приходится листать вручную».

Файлы: `tmux-web/static/js/telegram/chat.js` (DOM и замеры), `tmux-web/static/js/telegram/follow.js` (чистая логика следования), `tmux-web/static/js/telegram/format.js` (`isAtBottom`, `shouldLoadOlder`), `tmux-web/static/js/telegram/botkb.js` (кнопки бота), `tmux-web/static/js/telegram/main.js` (teardown).

---

## 1. Пять источников прокрутки

### 1.1 Безусловный доскролл вниз
`scrollToBottom()` = `$tgLog.scrollTop = $tgLog.scrollHeight`. Мгновенный, без анимации, без вопросов.
Где: вход в диалог (`openDialog` после `renderHistory`), отправка своего сообщения (`send`), строка ошибки (`errorLine`).
Правило: **своё действие пользователя всегда прижимает ленту к низу.** Спорить тут не с чем — он только что сам написал в этот чат.

### 1.2 Прилипание к низу (stick-to-bottom)
Замер `isAtBottom($tgLog)` (порог 40px от низа) → при истине `scrollToBottom()`.
Где: `onNewMessage` (было всегда), `onEdited` (**добавлено при исправлении дефекта — раньше там не было ни замера, ни доскролла**).
Правило: **замер обязан быть сделан ДО мутации DOM.** После `appendChild`/`replaceWith` высота ленты уже новая, и `isAtBottom` отвечает про ленту, которой пользователь ещё не видел. Это ровно тот случай, где «сначала нарисуем, потом померим» даёт молчаливо неправильный ответ, а не ошибку.

### 1.3 Якорь подгрузки истории вверх
`loadOlder`: запомнить `scrollHeight` → вставить страницу старых сообщений В НАЧАЛО → `scrollTop += прирост`. Без якоря лента прыгает: содержимое выросло сверху, а `scrollTop` остался прежним.
Триггер: `shouldLoadOlder($tgLog, {hasMore, loading})` — `scrollTop < 200 && hasMore && !loading`.
Правило: **внутри окна «замерил — вставил — восстановил» ленту не трогает НИКТО.** Флаг `local.loading` поднят на всё время загрузки, и `scrollBlockIntoView` (доскролл следования) при нём молча выходит. Посторонняя прокрутка здесь необратимо теряет позицию чтения — пользователя выбросило бы в случайное место истории.

### 1.4 Переход к оригиналу по цитате
`jumpToMessage` → `revealMessage` → `scrollRowIntoView(row)` (getBoundingClientRect, строка по центру) + `flashRow` (подсветка `tg-msg-highlight` на 2000мс).
Направление: **ВВЕРХ** — единственный источник, который уводит от низа ленты.
Правило: см. §2 — этот источник имеет приоритет над следованием.

### 1.5 Следование за ответом бота (новое)
Нажатие inline- или reply-кнопки включает окно 12с (`FOLLOW_TTL_MS`) на пару (диалог, message_id). Пока окно живо, правки ЭТОГО сообщения и новые сообщения в ЭТОМ диалоге доскролливают ленту так, чтобы КОНЕЦ блока был виден (`scrollTargetForRow` + зазор `FOLLOW_SCROLL_PAD` = 8px). Плавно (`smooth`), либо мгновенно при `prefers-reduced-motion: reduce`.
Направление: **только ВНИЗ** — `scrollTargetForRow` возвращает `null`, если цель выше текущего `scrollTop`.

---

## 2. Как они не мешают друг другу

| Пара | Кто выигрывает | Механизм |
|---|---|---|
| следование ↔ пассивное чтение истории | пассивное чтение | без следования решает `isAtBottom`; чужие правки и чужие сообщения ленту не двигают (`scrollDecision` ⇒ `'none'`) |
| следование ↔ прилипание к низу | следование | `scrollDecision` проверяет `following` ПЕРВЫМ: пользователь мог отлистать вверх, но результат СВОЕГО нажатия обязан увидеть |
| следование ↔ якорь подгрузки вверх | якорь | `scrollBlockIntoView` выходит при `local.loading === true` |
| следование ↔ переход по цитате | переход по цитате | ДВЕ независимые меры: `jumpToMessage` первым делом зовёт `stopFollow()`, а `scrollTargetForRow` не тянет вверх никогда — даже переживи следование этот вызов, оно не смогло бы утянуть ленту обратно вниз мимо цели |
| следование ↔ ручная прокрутка пользователя | пользователь | `bindScroll` → `cancelledByScroll(prev, next)` — движение ВВЕРХ сверх 24px гасит следование |
| следование ↔ чужой диалог | чужой диалог не трогаем | `onEdited` сверяет `activeDialogId`, `followsMessage`/`isFollowing` сверяют `dialogId` |
| следование ↔ смена диалога | смена диалога | `openDialog` зовёт `stopFollow()` — но только при РЕАЛЬНОЙ смене; переоткрытие того же чата ([retry]) следование сохраняет |

**Общий принцип:** явный интент пользователя (нажатие кнопки, своя прокрутка, клик по цитате) всегда сильнее автоматики, а более свежий интент сильнее более старого.

---

## 3. Жизненный цикл следования

```
клик по кнопке бота
  └─ botkb.js::runAction           (только 'callback' и 'send-text', ДО запроса)
      └─ onButtonPress             (колбэк, инжектируется из chat.js::initChat)
          └─ chat.js::noteButtonPress
              ├─ follow.js::noteFollow(dialogId, messageId, Date.now())
              ├─ armFollowTimer()            → setTimeout(FOLLOW_TTL_MS) → stopFollow
              └─ observeFollowRow(row)       → ResizeObserver.observe

пока окно живо:
  onEdited(msg)     → замеры ДО мутации → afterRowChanged → scrollBlockIntoView
  onNewMessage(msg) → замеры ДО вставки → afterRowChanged → scrollBlockIntoView
  ResizeObserver    → onFollowResize        → scrollBlockIntoView   (отложенный рост)

гасится (stopFollow):
  таймер 12с | прокрутка вверх >24px | смена диалога | удаление сообщения
  | jumpToMessage | resetChat (logout) | teardownChat (уход страницы)
```

**Почему регистрация ДО запроса:** правка сообщения приезжает ОТДЕЛЬНЫМ WS-кадром `message_edited` и вполне может опередить ответ на сам callback. Регистрация «после ответа» не срабатывала бы именно у быстрых ботов — то есть в самом частом случае.

**Почему таймер, если срок и так проверяется по часам:** `followTarget` не даст «протухшему» следованию сработать и без таймера; таймер нужен ради СНЯТИЯ ResizeObserver — иначе он висел бы на строке до следующей смены диалога.

---

## 4. Отложенный рост и наблюдатель размера

Рост высоты часто наступает НЕ в момент правки: догружается картинка, подменяется шрифт, бот перерисовывает клавиатуру и она занимает лишний ряд. Одного доскролла при замене строки для этого не хватает.

* **Один** `ResizeObserver` на всю вкладку (`ensureFollowObserver`, ленивое создание). По одному на клик было бы утечкой ровно того рода, которую ResizeObserver и призван закрывать.
* Меняется не наблюдатель, а **наблюдаемый узел**: `observeFollowRow` / `unobserveFollowRow`.
* `typeof ResizeObserver` проверяется В МОМЕНТ ВЫЗОВА ⇒ корректная деградация: без него теряется только ДОБОР отложенного роста, доскролл в момент правки продолжает работать.
* Наблюдение хранит УЗЕЛ (а не id, как подсветка `flashId`), поэтому каждая точка смерти узла обязана его переставить:

| Точка | Что делает | Почему |
|---|---|---|
| `clearLog()` | `unobserveFollowRow` | единственная точка, через которую проходят ВСЕ снесения ленты |
| `renderHistory()` | `observeFollowRow(rows.get(followTarget().messageId))` | лента перерисована целиком, все узлы новые (по образцу восстановления `flashId` строкой выше) |
| `afterRowChanged()` | `observeFollowRow(fresh)` | после `row.replaceWith(fresh)` старый узел вне документа, а рост ещё впереди |
| `onDeleted()` | `unobserveFollowRow` при `followRow === row` + `stopFollow` | строка переиспользована как заглушка `<< message purged >>` |
| `openDialog()` | `stopFollow` при смене диалога | следование принадлежит покинутому чату |
| таймер TTL | `stopFollow` | окно истекло |
| `onFollowResize` | самопроверка `followTarget` ⇒ `stopFollow` | наблюдатель может пережить истечение на кадр |
| `teardownChat()` | `disconnect()` + обнуление | конец жизни страницы (`main.js::teardownTelegram`) |

---

## 5. Доступность и деградация

* **prefers-reduced-motion**: `prefersReducedMotion()` читает медиазапрос НА КАЖДОМ доскролле (настройку можно переключить не перезагружая вкладку) под двойным typeof-гардом и в try/catch; `follow.js::scrollBehavior` ⇒ `'auto'` при reduce, иначе `'smooth'`. Прилипание к низу и переход по цитате мгновенны всегда — там анимации не было.
* **Element.scrollTo** вызывается под `typeof === 'function'` в try/catch, фолбэк — присваивание `scrollTop` (старые WebView и node-стенды).
* **Без ResizeObserver** фича теряет только добор отложенного роста.
* Все числовые входы `follow.js` прогоняются через `Number(...) || 0` / `Number.isFinite` — мусор в раскладке даёт no-op, а не `NaN` в `scrollTop`.

---

## 6. Что осталось неизменным (регресс-щиты)

Подгрузка истории вверх с якорем; безусловные `scrollToBottom` в `send()` и `errorLine()`; переход к оригиналу (`jumpToMessage` / `revealMessage` / `scrollRowIntoView` / `flashRow`); полоса ответа (reply-bar); reply-клавиатура бота; инлайн-плеер медиа. CSS не менялся — якорный элемент не понадобился, всё считается по `getBoundingClientRect`.

---

## 7. Тесты

| Файл | Что покрывает |
|---|---|
| `tests/frontend/telegram_follow.test.mjs` | чистая логика `follow.js` без DOM: срок, адресность, решение, цель доскролла, отмена, режим прокрутки |
| `tests/frontend/telegram_scroll_ui.test.mjs` | основной путь UI: доскролл в обеих ветках `onEdited`, `onNewMessage`, наблюдатель, prefers-reduced-motion, смена диалога, перерисовка ленты |
| `tests/frontend/telegram_scroll_edge.test.mjs` | 17 краевых секций, 149 ассертов; каждая мутация продуктового кода (13 штук) проверена на способность уронить тест |

## 8. Ключевые сущности одним списком

`follow.js`: `FOLLOW_TTL_MS`, `FOLLOW_SCROLL_PAD`, `FOLLOW_CANCEL_UP_PX`, `noteFollow`, `clearFollow`, `followTarget`, `isFollowing`, `followsMessage`, `scrollDecision`, `scrollTargetForRow`, `cancelledByScroll`, `scrollBehavior`, `_debugFollowState`.

`chat.js`: `noteButtonPress`, `armFollowTimer`, `stopFollow`, `ensureFollowObserver`, `observeFollowRow`, `unobserveFollowRow`, `onFollowResize`, `prefersReducedMotion`, `rowOffset`, `scrollBlockIntoView`, `afterRowChanged`, `teardownChat`, поля `local.followRow` / `local.followObserver` / `local.followTimer` / `local.lastScrollTop`.

`botkb.js`: колбэк `onButtonPress` в `initBotKeyboards`, вызов в `runAction`.

## ПЯТЫЙ ИСТОЧНИК: ОТЛОЖЕННЫЙ РОСТ СОДЕРЖИМОГО (br forge-ujck)

Лента дорастает уже ПОСЛЕ прокрутки: ленивые превью вложений (js/telegram/media.js) растят карточку с 92px до 184px через сотни миллисекунд после `scrollToBottom()`. Один синхронный доскролл этого не переживает — низ уезжает под кромку ровно на сумму догруженного (замер: 276px = 3 x 92), и дальше дефект самоусиливается: `isAtBottom` ложно, ветка `stick` не срабатывает больше никогда.

Поэтому прижатие к низу — ЗАЩЁЛКА `local.stickBottom`, а не разовое присваивание. `media.js::setMediaGrowthHandler` сообщает о каждом приросте (из события `load` на `<img>`, когда раскладка уже новая), `chat.js::onMediaGrew` возвращает ленту вниз.

ПРИОРИТЕТЫ ОТНОСИТЕЛЬНО ЧЕТЫРЁХ ОСТАЛЬНЫХ. Пятый источник — самый младший, у него три гарда:
- нет защёлки (пользователь читает историю) — не двигаем ничего;
- `local.loading` — якорь подгрузки вверх (источник 2) важнее: посторонний доскролл между замером `scrollHeight` и восстановлением `scrollTop` необратимо теряет позицию чтения;
- `followTarget(Date.now())` — следование (источник 4) важнее: у него своя цель (конец блока строки), прирост до неё доедет через `onFollowResize`.

Переход по цитате (источник 3) снимает следование и уезжает вверх, что через обработчик прокрутки снимает и защёлку — отдельного гарда ему не нужно.

Подробности — на странице `telegram-stickBottom`. Регресс: tests/frontend/telegram_scroll_stick.test.mjs.