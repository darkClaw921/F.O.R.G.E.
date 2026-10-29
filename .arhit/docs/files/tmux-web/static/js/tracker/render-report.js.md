# tmux-web/static/js/tracker/render-report.js

Режим `report` панели forge-tracker — экран сводки времени по макету ~/Documents/ForgeTracker.pen («Экран — ~/report»).

ЧТО ДЕЛАЕТ. Показывает за выбранный период: четыре KPI-блока (// TOTAL, // AVG/DAY, // CLOSED, // MAX FOCUS), разрез по дням (// BY DAY, символьная полоса, один знак = 15 минут), разрез по спискам (// BY LIST, относительная полоса и доля в процентах) и топ задач (// TOP NODES). Готовит CSV-выгрузку показанного.

ГЛАВНОЕ ПРАВИЛО: НИ ОДНА СУММА ЗДЕСЬ НЕ СКЛАДЫВАЕТСЯ. Все числа приходят из GET /api/tracker/report?from&to&group_by=day|task|list — вместе со строками сервер отдаёт поле kpi (plugins/tracker/src/report.rs::build_kpi). Правило локальных суток, правило идущего отрезка («считается до сейчас») и правило чек-листа (время пункта входит во время задачи ровно один раз) живут только в report.rs.

ЭКСПОРТИРУЕТ (чистые функции, покрыты tmux-web/tests/frontend/tracker_report.test.mjs):
- PERIODS ['day','week','month'], BAR_UNIT 900, BAR_WIDTH 16, LIST_BAR_WIDTH 12;
- windowFor(period, anchor) — полуоткрытое окно [from, to) с локальными границами суток; неделя с понедельника, месяц с первого числа;
- shiftAnchor(period, anchor, dir) — сдвиг окна встык;
- dayStart/addDays/dayKey/dateOfKey/shortDate/windowLabel — работа с локальными датами (dateOfKey не использует new Date('YYYY-MM-DD'), иначе день уезжает на сутки);
- fmtHm(sec) '21:47', fmtDelta(sec) '+3:12', dayRowLabel(key) 'пн 01.09', pct(sec,total);
- dayBar(sec) — абсолютная полоса {text, filled, over}; shareBar(sec, max) — относительная полоса '▓';
- kpiCards(kpi, view) — четыре карточки с подписями (подпись MAX FOCUS = день недели + тег или заголовок задачи);
- isEmptyReport(view) — признак пустого периода;
- reportToCsv(view), csvFileName(win) — CSV: разделитель ';', UTF-8 + BOM (иначе Excel с русской локалью читает как cp1251), время Ч:ММ:СС и отдельная колонка целых минут для сводных таблиц;
- renderReport(container, view) — отрисовка; пустой период даёт отдельное состояние .trk-r-none, а не четыре нуля.

СВЯЗИ: fmtDuration импортируется из render-list.js; вызывается из js/tracker/main.js (renderReportMode/refreshReport/exportReportCsv); стили — tmux-web/static/css/tracker.css (блок «Режим report»); разметка кнопок периода — #trk-report-nav в static/index.html. Никакого innerHTML: имена списков и заголовки задач попадают в DOM только через textContent.
