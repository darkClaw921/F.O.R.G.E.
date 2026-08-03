# tmux-web/static/js/vk/dialogs.js

Колонка бесед панели VK: список filter=all, секция четырёх СЕРВЕРНЫХ фильтров,
присутствие, счётчики непрочитанного.

Экспорты: initDialogs({onSelect}), loadDialogs, loadFilters, selectDir,
loadDirDialogs, mergeDialogPage, renderAll, renderDirs, findDialog, selectDialog,
highlightActive, clearActive, setUnread, bumpUnread, presenceOf,
renderOnlineCounter, clearPresence, resetDialogs, _debugDialogsState.
Realtime: onNewMessage, onRead, onOnline, onMuted, onDialogChanged.

Модуль НЕ импортирует chat.js: выбор беседы уходит колбэком onSelect, и граф
модулей остаётся ациклическим (единственная точка сборки — main.js).

onDialogChanged({dialog_id}) — обработчик НОВОГО кадра VK (события LongPoll
51/52). ЧЕСТНОЕ ОГРАНИЧЕНИЕ: кадр несёт dialog_id затем, чтобы фронт решил,
стоит ли вообще перечитывать (про беседу вне загруженной страницы и вне открытой
ленты знать нечего, и запрос ради неё пропал бы зря при бюджете 3 rps), но роута
«одна беседа» у бэкенда нет — есть только страница GET /api/vk/dialogs, поэтому
перечитывается она. Событий 51/52 в живой беседе приходит пачками
(переименовали, сменили фото, вошёл участник), поэтому чтение идёт с дебаунсом
DIR_DEBOUNCE_MS и склейкой всех накопившихся id в одно обращение.

Счётчик секции фильтров ВСЕГДА считается по vkState.dialogs (выборка filter=all)
и показывается только у ~/all и ~/unread: считать его по транзиентному списку
означало бы, что число меняется от того, куда пользователь ткнул.

## Белый список видимости (Ф10) — ДВЕ из четырёх точек применения

visibleSource() (колонка) и renderDirs() (счётчики) прогоняют список через
visibility.js::visibleDialogs с фильтром из state.js::currentVisibilityFilter.
Два независимых сита: серверный фильтр решает, ИЗ КАКОГО списка берём
(all — постоянный vkState.dialogs, остальные — транзиентный vkState.dirDialogs),
а белый список — какие беседы devforge вообще показывает. Белый список
применяется в visibleSource ОДНОЙ точкой на обе ветки: иначе беседа, скрытая в
~/all, всплывала бы в «Важных». Счётчики обязаны считать по тому же срезу, что
виден в колонке, иначе цифра ведёт в никуда. Ещё две точки живут в main.js
(бейдж и тосты) — пропуск любой из четырёх = протечка. Приглушения пустых
пунктов (телеграмный emptyDirKeys) здесь нет и быть не может: пустоту
серверного фильтра клиент не знает, не сделав запрос. Тест:
tmux-web/tests/frontend/vk_dialogs_filter.test.mjs. См. [[vk-visibility-model]].

СТРОКА СПИСКА ПОСЛЕ ПРАВКИ ИМЁН: первая строка это «Имя @ник [BOT] [M]» — displayName в .vk-dialog-title основным цветом и displayNick в .vk-dialog-nick приглушённым (--v-faint-text). Узла ника нет, когда ника нет. Подсказка (li.title) = tooltipText(d) = «Имя (@ник)» либо только имя.
