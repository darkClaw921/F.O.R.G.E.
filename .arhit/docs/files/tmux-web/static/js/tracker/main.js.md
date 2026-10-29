# tmux-web/static/js/tracker/main.js

Оркестратор панели трекера: состояние (local), выборки, режимы list/kanban/gantt/report, prompt-строка, WS, тикер, статус-бар.

ВЕХИ ГАНТТА больше не заводятся через window.prompt. addMilestone/editMilestone — тонкие обёртки над openMilestoneDialog(ms|null, midWindowDate()) из ./milestone.js; сам CRUD (create/patch/delete + ошибки + удаление явной кнопкой DELETE) живёт в диалоге, узлы и сервисы которого main.js передаёт в initTrackerMilestone (как и для диалога переноса initTrackerPush). Диалог закрывается в suspendTracker и снимается в teardownTracker.

Дата вехи по умолчанию — середина текущего окна Гантта (midWindowDate). Своей перерисовки после правки нет: сервер шлёт кадр reload, панель перечитывает снимок, снимок тянет refreshGantt.
