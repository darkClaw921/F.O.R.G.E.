# ensureFollowObserver

ensureFollowObserver() -> ?ResizeObserver (tmux-web/static/js/telegram/chat.js, внутренняя) — лениво создаёт ЕДИНСТВЕННЫЙ на всю вкладку наблюдатель размера строки.

ПОВЕДЕНИЕ: если local.followObserver уже есть — возвращает его; если `typeof ResizeObserver !== 'function'` — возвращает null; иначе создаёт `new ResizeObserver(onFollowResize)` в try/catch (при исключении — null).

ОДИН НА ВСЕХ, А НЕ ПО ОДНОМУ НА НАЖАТИЕ. Пользователь жмёт кнопки бота десятками (меню → раздел → страница → назад), и «новый ResizeObserver на каждый клик» — это утечка ровно того рода, которую ResizeObserver и призван закрывать. Поэтому наблюдатель переиспользуется, а меняется только НАБЛЮДАЕМЫЙ УЗЕЛ (observe/unobserve в observeFollowRow / unobserveFollowRow). Распускается наблюдатель ровно один раз — в teardownChat через disconnect().

ПОЧЕМУ typeof ПРОВЕРЯЕТСЯ В МОМЕНТ ВЫЗОВА, А НЕ НА ИМПОРТЕ. Во-первых, окружение может отличаться от того, что было при загрузке модуля (node-стенды тестов ставят шпион ResizeObserver уже после импорта). Во-вторых, это и есть заявленная «корректная деградация»: в среде без ResizeObserver (старый WebView) фича теряет только ДОБОР отложенного роста — доскролл в момент правки/вставки строки продолжает работать полностью, потому что он идёт из afterRowChanged, а не из наблюдателя.

КОЛБЭК: onFollowResize — перепроверяет срок следования и зовёт scrollBlockIntoView(local.followRow).

СВЯЗАННЫЕ: observeFollowRow, unobserveFollowRow, teardownChat, поле local.followObserver.
