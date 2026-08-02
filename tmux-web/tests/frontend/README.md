# Frontend regression tests

Standalone Node-based regression tests for `tmux-web/static/*.js` (включая
PWA-слой `static/sw.js` и `static/js/pwa/*`).
No package manager / no jest — pure Node, just run the file directly.

## Running

```bash
node tmux-web/tests/frontend/sidebar_grouping.test.js
node tmux-web/tests/frontend/sw.test.js
node tmux-web/tests/frontend/pwa_push_helper.test.js
node tmux-web/tests/frontend/pwa_mobile_helper.test.js
node tmux-web/tests/frontend/pwa_bootstrap_optin.test.js
node tmux-web/tests/frontend/pwa_register.test.mjs   # ESM-вариант (.mjs)
node tmux-web/tests/frontend/telegram_format.test.mjs # ESM-вариант (.mjs)
node tmux-web/tests/frontend/telegram_reply.test.mjs    # чистая логика ответов
node tmux-web/tests/frontend/telegram_reply_ui.test.mjs # UI ответов на фейковом DOM
node tmux-web/tests/frontend/telegram_follow.test.mjs    # чистая логика автопрокрутки
node tmux-web/tests/frontend/telegram_scroll_ui.test.mjs # автопрокрутка на фейковом DOM
node tmux-web/tests/frontend/telegram_scroll_edge.test.mjs # автопрокрутка, краевые углы
node tmux-web/tests/frontend/telegram_keys.test.mjs      # чистая логика горячих клавиш
node tmux-web/tests/frontend/telegram_keynav.test.mjs    # клавиатурная навигация на фейковом DOM
node tmux-web/tests/frontend/telegram_visibility.test.mjs      # чистая логика белого списка чатов
node tmux-web/tests/frontend/telegram_dialogs_filter.test.mjs  # белый список в колонке чатов
```

Exit code 0 — all assertions pass.
Exit code 1 — at least one assertion failed (see `[FAIL ...]` lines).

> `.mjs`-файлы — для тестов, грузящих РЕАЛЬНЫЙ ES-модуль через `await import()`
> (register.js). Команда запуска та же — `node <file>`.

## Files

| File | What it covers |
| --- | --- |
| `sidebar_grouping.test.js` | Phase 6 / forge-cca8.2 — группировка sessions внутри origin'а, двухуровневая фильтрация origin → project, contract `aggregateAllOrigins`, projectFilter не сбрасывается при смене origin. |
| `sw.test.js` | PWA Service Worker (`static/sw.js`) — грузит РЕАЛЬНЫЙ `sw.js` в `node:vm`-песочнице с моками `self`/`caches`/`fetch`/`clients`/`Request`/`Response`. Краевые случаи fetch-роутинга (не-GET, `/ws/*`, upgrade=websocket, cross-origin, navigate network-first + fallback, статика SWR, data allowlist + boundary, прочие `/api/*` и `/healthz` без кэша), install precache (`addAll` `{cache:'reload'}`, атомарность), activate cleanup (только `forge-*` не текущей версии + `clients.claim`), message SKIP_WAITING, push (JSON/text/без data/частичный payload), notificationclick (focus/navigate/openWindow/гварды). |
| `pwa_push_helper.test.js` | PWA push helper — грузит РЕАЛЬНУЮ `urlBase64ToUint8Array` из `static/js/pwa/push.js` через `await import()` + cross-check с локальной репликой. 65-байтный VAPID-ключ, padding по mod4, замена `-_`→`+/`, пустая строка, инвариант длины, бросок на невалидном base64, чистота функции. |
| `pwa_mobile_helper.test.js` | PWA mobile pure-логика (РЕПЛИКА из `static/js/pwa/mobile.js`, т.к. он импортит DOM/xterm) — `countNeedsAttention`, `keyboardHeight`, маршрутизация `updateBadge` (setAppBadge/clearAppBadge/фолбэк/дедуп/проглатывание ошибки), feature-guard-предикаты (отсутствие API → early-return), `safe()`-изоляция фич. |
| `pwa_bootstrap_optin.test.js` | PWA opt-in gate (`static/js/pwa/bootstrap.js`) — грузит РЕАЛЬНЫЙ `bootstrap.js` в `node:vm`-песочнице (синтаксис `import(` → мок `__dynImport(`). Краевые случаи строгого opt-in: config 404 / `enabled:false` / сетевая ошибка fetch / `enabled` truthy-но-не-`true` → `disablePwa` (unregister всех SW + удаление ТОЛЬКО `forge-*` кэшей, без инжекта в `<head>`); отсутствие `serviceWorker`/Cache API → без исключения; `enabled:true` → `window.__FORGE_PWA`+vapidPublicKey, инжект manifest/theme-color/apple-meta/apple-touch-icon/pwa.css, импорт register.js + `registerServiceWorker()`; идемпотентность инжекта против предсуществующей разметки. |
| `pwa_register.test.mjs` | PWA SW update-flow — грузит РЕАЛЬНЫЙ `static/js/pwa/register.js` через `await import('...?bust=N')` (свежий модуль на сценарий) с моками globalThis (`navigator`/`window`/`document`/`requestAnimationFrame`/`setTimeout`). Первая установка без баннера, `reg.waiting`+controller → баннер, updatefound→installed+controller, SKIP_WAITING postMessage, controllerchange → ровно один reload (guard `refreshing`), register reject → warn+null, идемпотентность/dismiss баннера, структура DOM. |
| `telegram_format.test.mjs` | Вкладка Telegram (Фаза 3) — грузит РЕАЛЬНЫЙ `static/js/telegram/format.js` через `await import()` (модуль намеренно без DOM/core-импортов). Слаги имён (`sofia@node`, вычистка разметки, обрезка), статус-строки `[ONLINE]`/`[IDLE] last 2h`/`[GROUP] N msgs` (счётчик msgs только у групп/каналов), разделение узлы/каналы, суммарный unread с учётом muted и `99+`, `HH:MM:SS`/`dayKey`/разделители дат на границе суток, `keyHex`+шапка сессии, ники (`you@local`, @username, title канала), **дедупликация `insertMessage` по message_id** (своё сообщение, пришедшее вторым разом через WS), склейка страниц `prependMessages` без пересечений, `oldestId` как курсор `before_id`, stick-to-bottom и порог подгрузки вверх, троттлинг тостов 5с, оценка задержки по дрейфу серверных ping'ов. |
| `telegram_reply.test.mjs` | Ответы на сообщения (reply), ЧИСТАЯ логика — грузит РЕАЛЬНЫЙ `static/js/telegram/reply.js` через `await import()` (модуль без DOM и сети, зависит только от `format.js`). Нормализация `MessageDto.reply_to` во всех трёх формах (полное превью / только `message_id` из потока апдейтов / голое `reply_to_id`) и приоритет `reply_to` над `reply_to_id` на форум-топиках, превью цитаты (текст, вложение без подписи и с подписью, процитированный фрагмент, удалённый оригинал, ответ на сторис, ответ в чужой чат), обрезка по code points с многоточием ВНУТРИ лимита и без одиноких суррогатов, имя автора по `out` (`you@local`) и краевые имена (пробелы, эмодзи, длина), локальная досборка превью из ленты с защитой от чужого id, состояние «на что отвечаем» с автосбросом по диалогу, классификация клика по строке (плеер, кнопки бота, ссылки, цитата). |
| `telegram_reply_ui.test.mjs` | Ответы на сообщения (reply), UI — грузит РЕАЛЬНЫЕ `chat.js` и `upload.js` поверх фейкового DOM (дерево, `classList`/`dataset`, `closest`/`querySelector`, делегирование событий) + стабы `fetch` и `XMLHttpRequest`. Полоса ответа над `#tg-prompt` (имя/превью/✕, позиция вплотную к форме даже после вклинившейся клавиатуры бота, фокус в поле), отмена по Esc и приоритет лайтбокса, двойной клик по строке и стоп-лист (кнопки бота, ссылки, плеер, системные строки, карточка отправки), кнопка «ответить» (aria-label, последний потомок, `stopPropagation`), цитата в строке и переход к оригиналу (скролл + подсветка по id, переживающая перерисовку; догрузка одним запросом; «удалено» vs обрыв связи; гард от повторного перехода), отправка текста (`reply_to_id` в теле, цитата в optimistic-строке ДО подтверждения, полоса держится до успеха, `[retry]` в то же сообщение, явный `null`), автосброс цели (удаление оригинала, смена диалога, logout), отправка МЕДИА (`reply_to_id` у всех файлов пачки, фиксация цели на момент постановки в очередь, отвергнутый файл цель не снимает, явный `opts.replyToId` полосу не гасит, чужой диалог). |
| `telegram_follow.test.mjs` | Автопрокрутка ленты после нажатия кнопки бота, ЧИСТАЯ логика — грузит РЕАЛЬНЫЙ `static/js/telegram/follow.js` через `await import()` (модуль без DOM и сети). Окно «следования» за парой (диалог, message_id) и его границы, скачок часов назад, адресность (чужой диалог / чужое сообщение / тот же message_id в соседнем чате), перецеливание и продление пачкой нажатий, следование за диалогом целиком у кнопок reply-клавиатуры (`messageId == 0`), решение `scrollDecision` (`follow` / `stick` / `none`), расчёт цели прокрутки `scrollTargetForRow` (зазор, потолок `scrollHeight - clientHeight`, «вверх не тянем никогда», лента короче экрана, неизвестная раскладка), отмена ручной прокруткой по НАПРАВЛЕНИЮ с порогом, выбор режима по prefers-reduced-motion. |
| `telegram_scroll_ui.test.mjs` | Автопрокрутка ленты, UI — грузит РЕАЛЬНЫЙ `chat.js` поверх фейкового DOM (тот же срез, что в `telegram_reply_ui`, плюс `scrollTo` с журналом режимов, переключаемый `window.matchMedia`, шпион `ResizeObserver` и флаг «строки выше экрана»). Прежнее поведение без нажатия (читающего историю не дёргаем, прилипание к низу работает), нажатие инлайн-кнопки включает следование и берёт строку под наблюдение, доскролл после правки бота в ОБЕИХ ветках `onEdited` (полная пересборка с клавиатурой и точечный патч текста), ответ бота НОВЫМ сообщением, добор отложенного роста через `ResizeObserver`, smooth/auto по prefers-reduced-motion и фолбэк без `scrollTo`, отмена следования прокруткой вверх, истечение TTL (самоснятие наблюдателя), «наблюдателей не плодим», снятие наблюдения при смене диалога / перерисовке истории / удалении сообщения / `resetChat` / `teardownChat`, деградация без `ResizeObserver`, регресс-щит «переход по цитате по-прежнему центрирует строку, а не улетает вниз». |
| `telegram_scroll_edge.test.mjs` | Автопрокрутка ленты, КРАЕВЫЕ углы — свой процесс и свой фейковый DOM (стенд `telegram_scroll_ui` плюс построчная модель высоты: `getBoundingClientRect` отчитывается «строка выше экрана» только для id из `tallRows`, поэтому одно сообщение растёт, соседнее остаётся коротким, а выросшее умеет сжаться обратно; плюс `window.open` для url-кнопок). Рост БЕЗ нажатия (внизу — прежнее прилипание, при чтении истории — тишина), нажатие из середины истории, дребезг прокрутки против настоящего ухода вверх, ответ бота НОВЫМ сообщением и второй подряд, ответ через 20 с (окно истекло — поведение ровно как до фичи), две кнопки в разных сообщениях подряд, правка в НЕактивном диалоге и совпадение `message_id` между чатами, сообщение стало КОРОЧЕ (ленту не выкидывает и вверх не тянет), подгрузка истории вверх (якорь чтения не сбивается ни доскроллом, ни наблюдателем), prefers-reduced-motion с переключением МЕЖДУ вызовами, кнопка reply-клавиатуры, жизненный цикл наблюдателя (выпадение цели из ленты, истечение окна по часам при не выстрелившем таймере, удаление сообщения, смена диалога, `teardownChat` и жизнь ПОСЛЕ него), окружение без `ResizeObserver` и без `scrollTo`, `open-url`/`inline-query` следование не включают, гонка «правка обогнала ответ на callback», ошибка нажатия (flood_wait) следование не гасит, запоздалое срабатывание наблюдателя после отмены, регресс-щит «своя отправка по-прежнему безусловно уезжает в самый низ». |
| `telegram_keys.test.mjs` | Горячие клавиши Telegram, ЧИСТАЯ логика — грузит РЕАЛЬНЫЙ `static/js/telegram/keys.js` через `await import()` (модуль без DOM и импортов панели). Разбор события в каноническую комбинацию по `event.code` (главное: русская раскладка даёт ту же клавишу, что латинская), фолбэк на `event.key` без кода, одиночный модификатор комбинацией не считается, канонический порядок модификаторов и синонимы (`cmd`/`command`/`control`/`option`), «?» как `Shift+/`, подписи для UI (`⇧G`, `⏎`, `Esc`), реестр действий (уникальность id и дефолтов, `fixed` у Escape), разрешение переопределений (переопределение сильнее дефолта, проигравший конфликт остаётся БЕЗ клавиши, результат не зависит от порядка ключей в JSON, мусор и чужие id отбрасываются), вычисление того, что слать на backend (только отличия от дефолта), правило «в поле ввода одиночные клавиши не перехватываем, Ctrl/Alt/Meta и Escape — да». |
| `telegram_keynav.test.mjs` | Клавиатурная навигация по панели Telegram, UI — грузит РЕАЛЬНЫЙ `keynav.js` поверх фейкового DOM (составные селекторы `#tg-dialog-list .tg-dialog`, настоящий `document.activeElement`, стаб буфера обмена). Гейты владения клавиатурой (скрытая панель, оверлей логина, выключенный тумблер, фокус ВНЕ панели возвращает vim-навигацию сессий) и публикация контракта `ForgeApp.telegramKeys` для `hotkeys.js`; курсор по чатам (обе колонки как один список, края, переживание перерисовки колонки по id, Enter открывает, `u` по непрочитанным по кругу, `[`/`]` по папкам); курсор по ленте (`m`, `j`/`k`, прокрутка вверх на первом сообщении вместо тупика, `⇧G` уводит и ленту, и курсор); действия над сообщением (`r` ответ + фокус в поле, `y` копирование, `o` переход к цитате, Enter открывает медиа, `b` фокус на кнопку бота и Enter по ней, честная подсказка когда кнопок нет); поле ввода (одиночные клавиши и `⇧G` НЕ перехватываются, Escape возвращает в ленту, но при активной цели ответа отдан chat.js); шпаргалка (`?` открывает/закрывает, Escape закрывает, из поля ввода не открывается, показывает переназначенные комбинации); применение переназначений и конфликтов без перезагрузки; молчание при открытом лайтбоксе и модалке настроек; `resetKeynav`/`teardownKeynav` и повторный `initKeynav` без второго слушателя. |
| `telegram_toasts.test.mjs` | Уведомления и бейдж непрочитанного — грузит РЕАЛЬНЫЙ `main.js` со стабом уровня обработчиков (тосты считаются по выводу `console.log('[toast/…')`, контейнер отсутствует). Гейт «звенит ли диалог» по полям кадра `new_message` (muted/archived сервера сильнее локального знания, старый сервер без полей — фолбэк), накопление непрочитанного по чатам вне загруженной страницы и его снятие, пересчёт бейджа, битые кадры. Плюс белый список видимости (forge-c2dt): чат вне выбора не звенит и не считается, неизвестный чат при действующем фильтре молчит, поимённый выбор работает без загруженного DTO, накопленное по спрятанному чату снимается, включённый тумблер с пустым выбором ведёт себя как выключенный, заглушённость и архив сильнее видимости. |
| `telegram_visibility.test.mjs` | Белый список видимых чатов и папок, ЧИСТАЯ логика — грузит РЕАЛЬНЫЙ `static/js/telegram/visibility.js` через `await import()` (зависит только от `format.js`). Разбор настроек в рабочий фильтр и правило «включён, но ничего не выбрано = фильтр НЕ действует» (иначе пустая панель читалась бы как поломка), сохранение выбора при выключенном тумблере, мусор в списках (пробелы, дубли, числовые id, не-массив), видимость по поимённому выбору и по папкам (`include_ids`, категорийная папка ботов, служебные `inbox`/`archive`), объединение папок и чатов, исчезнувшая или ещё не загруженная папка (совпадений не даёт, но из настроек не пропадает), строгость к чатам вне загруженной страницы (`dialogIdVisible`: неизвестный чат скрыт, поимённый выбор работает и без DTO), пометка пустых пунктов навигации, склонения в сводке для UI (1/2–4/5+/11/21), чистка исчезнувших чатов. |
| `telegram_dialogs_filter.test.mjs` | Белый список в колонке чатов, UI — грузит РЕАЛЬНЫЙ `dialogs.js` поверх фейкового DOM. Что попадает в колонки узлов и каналов при выключенном фильтре, при выборе папки, поимённом выборе и их сочетании; счётчики папок считают по ВИДИМОМУ срезу (цифра по всему списку вела бы в скрытый чат); пункт без видимых чатов помечается `tg-dir-filtered-out`, но из навигации не исчезает; служебные `inbox`/`archive` в белом списке; архивный чат виден только при открытом архиве; скрытый чат не всплывает в чужой папке; исчезнувшая папка в списке и удалённая активная папка (откат на inbox) рендер не роняют; смена настроек действует сразу после `invalidateVisibilityFilter` + `renderAll`, без перезагрузки страницы. |

## Контракт

Тесты реплицируют **pure-логику** из `static/app.js` (без DOM). Это значит,
что при изменении любой из функций ниже — нужно одновременно поправить и
ассерты в тестовых файлах:

- `groupSessionsByProject(sessions, orphanKey)` — экспортирована в
  `window.__forge.groupSessionsByProject`;
- `aggregateAllOrigins()` — экспортирована в `window.__forge.aggregateAllOrigins`;
- логика "какие origin'ы видны" в `renderSidebarWithOrigin`;
- логика двухуровневой фильтрации в `renderOriginSection`.

Если упал тест после правки `app.js` — это сигнал что меняется публичный
контракт, проверь что:
1. backend/UI остаются совместимы;
2. документация в `arhit doc` обновлена;
3. legacy режим (`remote_mode=false`, `renderSidebar` без `renderSidebarWithOrigin`)
   не задет — это явное требование Phase 6.

### PWA-контракты

`sw.test.js` и `pwa_register.test.mjs` грузят **РЕАЛЬНЫЙ** исходник
(`static/sw.js`, `static/js/pwa/register.js`) — они ловят дрейф автоматически:
любое изменение fetch-роутинга / lifecycle / update-flow, ломающее контракт,
валит тест. То же для `urlBase64ToUint8Array` в `pwa_push_helper.test.js`
(грузится через `import()` из `static/js/pwa/push.js` + сверка с репликой).

`pwa_mobile_helper.test.js` использует **РЕПЛИКУ** pure-логики (mobile.js нельзя
импортировать в Node — он тянет `core/state.js` + `terminal/xterm.js` с
DOM/WebSocket/xterm top-level). При правке `mobile.js` синхронно обнови реплики
в тесте:

- `countNeedsAttention(data)` — `Array.isArray(data) ? data.filter(s => s && s.needs_attention).length : 0` (mobile.js строки 136-138);
- `keyboardHeight(innerHeight, vvHeight, vvOffsetTop)` — `Math.max(0, Math.round(innerHeight - vvHeight - vvOffsetTop))` (mobile.js строки 78-81);
- `updateBadge`-маршрутизация App Badge API (mobile.js строки 163-175);
- feature-guard-предикаты `'setAppBadge' in navigator` / `!!window.visualViewport` / `'wakeLock' in navigator` и `safe()`-изоляция фич.

Контракт SW push-handshake между `register.js` и `sw.js`: сообщение строго
`{type:'SKIP_WAITING'}` (проверяется в обоих файлах) — расхождение в имени type
рвёт обновление.
