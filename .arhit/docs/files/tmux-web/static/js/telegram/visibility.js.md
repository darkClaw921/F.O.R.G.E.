# tmux-web/static/js/telegram/visibility.js

Белый список видимых чатов и папок вкладки Telegram (forge-c2dt). НАМЕРЕННО чистый модуль: ни DOM, ни сети, ни состояния — зависит только от format.js (dialogInFolder), поэтому целиком покрывается node-тестом без фейкового документа.

ЧТО ДЕЛАЕТ ФИЧА. Пользователь отмечает в настройках папки и/или отдельные чаты. Когда режим включён, во вкладке видны ТОЛЬКО они: и в колонке чатов, и в счётчиках папок, и в бейдже непрочитанного на ✈, и в тостах. Остальные чаты продолжают жить в аккаунте — devforge просто перестаёт их показывать.

ПОЧЕМУ ФИЛЬТР «ДЕЙСТВУЕТ», А НЕ ПРОСТО «ВКЛЮЧЁН». Тумблер и непустой выбор — разные вещи. Включённый режим с пустым белым списком означал бы пустую панель, и пользователь прочитал бы это как поломку, а не как собственную настройку. Поэтому filterActive требует ОБОИХ условий (enabled && (folders.length || dialogs.length)), а форма настроек показывает предупреждение, пока ничего не выбрано.

СТРОГОСТЬ К НЕИЗВЕСТНЫМ ЧАТАМ. WS вещает по ВСЕМУ аккаунту, а /dialogs отдаёт только верхушку списка. Про чат из кадра new_message может быть неизвестно ничего, кроме id. При действующем фильтре такой чат считается СКРЫТЫМ (кроме случая, когда он отмечен поимённо): иначе фильтр протекал бы уведомлениями ровно от тех чатов, которые пользователь просил не показывать. Цена — чат из отмеченной папки, не попавший в загруженную страницу, тоже промолчит до её догрузки.

ЭКСПОРТЫ. DIR_INBOX / DIR_ARCHIVE / DIR_FOLDER_PREFIX; folderKey(folder) → 'folder:<id>'; folderByKey(key, folders); normalizeFilter(settings) → {enabled, folders, dialogs, folderSet, dialogSet, active}; filterActive(filter); dialogVisible(d, filter, folders); visibleDialogs(dialogs, filter, folders) (возвращает КОПИЮ — вызывающий её режет и сортирует); dialogIdVisible(id, filter, dialogs, folders); emptyDirKeys(entries, dialogs, filter, folders); filterSummary(filter) (со склонениями); pruneMissingDialogs(selected, dialogs).

ПОРЯДОК ПРОВЕРОК в dialogVisible: поимённый выбор сильнее всего (отмеченный чат виден, даже если его папка не отмечена), затем папки. 'inbox' в белом списке значит «все неархивные», 'archive' — «весь архив». Исчезнувшая (удалённая на другом устройстве) или ещё не загруженная папка просто не даёт совпадений, но из настроек НЕ вычищается — она может вернуться со следующим folders_changed.

pruneMissingDialogs зовётся ТОЛЬКО из формы настроек по кнопке и только по свежезагруженному полному списку: чистить на каждом рендере нельзя — страница /dialogs отдаёт верхушку, и выбранный, но давно не поднимавшийся чат вычистился бы сам собой.

Потребители: dialogs.js (renderAll, renderDirs), main.js (badgeUnread, dialogShown → тосты и pendingUnread), state.js (currentVisibilityFilter с кэшем), settings/telegram-visibility-tab.js. Тесты: telegram_visibility.test.mjs (75 ассертов), telegram_dialogs_filter.test.mjs (36), секция в telegram_toasts.test.mjs.
