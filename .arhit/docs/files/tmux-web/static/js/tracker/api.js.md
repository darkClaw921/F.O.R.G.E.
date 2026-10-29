# tmux-web/static/js/tracker/api.js

Клиент REST /api/tracker/* панели «Мои задачи».

Тонкая обёртка без собственного состояния: снимок живёт в js/tracker/main.js, живые обновления приходят через js/tracker/ws.js. Модуль знает только адреса роутов (таблица заморожена в plugins/tracker/src/routes/mod.rs) и единый конверт ошибок {"error":{"code","message"}} (plugins/tracker/src/routes/error.rs).

Класс TrackerApiError(status, code, message): code — bad_request | not_found | conflict | internal | http | network. Все функции, кроме getStatus, бросают его на неуспешном ответе.

ГЕЙТ — ЕДИНСТВЕННОЕ ИСКЛЮЧЕНИЕ: getStatus() возвращает null при 404 и при любой сетевой беде и НЕ пишет в консоль, потому что плагин opt-in и его отсутствие штатно. Во всех остальных функциях 404 значит «объекта с таким id нет», и молчание спрятало бы расхождение состояния.

Поверхность (24 роута): getState; задачи listTasks/getTask/createTask/patchTask/deleteTask/setTaskStatus/reorderTasks; таймеры startTimer/stopTimer (параллельные таймеры по разным задачам штатны, повторный старт по той же задаче возвращает идущий отрезок); отрезки listEntries/createEntry/patchEntry/deleteEntry; списки listLists/createList/patchList/deleteList/reorderLists; чек-лист listItems/createItem/patchItem/deleteItem/toggleItem/reorderItems/startItemTimer/stopItemTimer; вехи listMilestones/createMilestone/patchMilestone/deleteMilestone; отчёты getReport/getGantt.

Авторизация здесь не делается: window.fetch подменён в js/core/auth.js и сам добавляет Authorization: Bearer к same-origin запросам.
