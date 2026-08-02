# dialogs_default_limit

plugins/telegram/src/routes/dialogs.rs — выбор дефолтного limit для GET /api/telegram/dialogs.

Сигнатура: dialogs_default_limit(folder: Option<&str>) -> usize.

Правило: явный ?folder= (в том числе folder=all) ⇒ DEFAULT_FOLDER_DIALOGS = 300; запрос БЕЗ
параметра ⇒ прежний DEFAULT_DIALOGS = 100. Пустая строка трактуется как отсутствие параметра
(так же, как в parse_folder). Потолок ?limit= остался прежним: MAX_DIALOGS = 500.

ПОЧЕМУ АСИММЕТРИЯ: первая отрисовка колонки диалогов после логина не должна замедляться
несколькими лишними RPC, а вот расширенная выборка нужна ровно тогда, когда фронт собирается
фильтровать список по пользовательской папке — иначе редкий чат с ботом отсекается первой сотней
(вторая половина D3).

Соотношения констант проверяются компилятором, а не тестом:
const _: () = assert!(DEFAULT_FOLDER_DIALOGS > DEFAULT_DIALOGS);
const _: () = assert!(DEFAULT_FOLDER_DIALOGS <= MAX_DIALOGS);
const _: () = assert!(MAX_FOLDER_PAGES * FOLDER_PAGE >= MAX_DIALOGS);

Парный фронтовый параметр: format.js::FOLDER_LIST_LIMIT = 300 (dialogs.js::ensureFullList).
ВАЖНО: ?folder= — это PEER-папка Telegram (main|archive|all), а НЕ id пользовательской
папки-dialogFilter. Пользовательские папки по-прежнему фильтруются на фронте.
