# js/vk/dialogs.js

tmux-web/static/js/vk/dialogs.js — левая колонка #vk-contacts (Ф4.9). Покрыт tests/frontend/vk_dialogs.test.mjs (59 проверок на фейковом DOM).

ГЛАВНОЕ ОТЛИЧИЕ ОТ telegram/dialogs.js: «директория» у VK — это ЗАПРОС, а не фильтр загруженного массива. Следствия, видимые в коде: selectDir переносит подсветку сразу, а запрос уходит с дебаунсом DIR_DEBOUNCE_MS=300 через общий vkLimiter+vkCoalescer; ДВА СПИСКА в состоянии — vkState.dialogs всегда filter=all (источник бейджа и счётчиков) и транзиентный vkState.dirDialogs (иначе бейдж считал бы по случайной выборке); emptyDirKeys НЕ переносится — пустоту серверного фильтра клиент не знает, не сделав запрос.

Ответ покинутого фильтра отбрасывается по поколению (createGeneration): отмена не мгновенна, и без проверки колонка показала бы «Важные» под заголовком «Непрочитанные».

Функции: initDialogs({onSelect}) (идемпотентна, делегирование на #vk-dir-list), loadDialogs (ВСЕГДА filter=all), loadFilters (best-effort, встроенный список из 4 пунктов как запасной), selectDir, loadDirDialogs, renderAll/renderDirs/renderList/buildRow, findDialog (смотрит ОБА списка), selectDialog/clearActive/highlightActive, setUnread/bumpUnread (патчат оба списка), onMuted (у VK глушение только per-peer — notifyDefaults нет), onOnline/presenceOf/renderOnlineCounter/clearPresence, onRead, onNewMessage, resetDialogs, _debugDialogsState.

ИНВАРИАНТ ГРАФА: chat.js НЕ импортируется — onSelect приходит колбэком из main.js. XSS: ни одного innerHTML, только createElement + textContent.
