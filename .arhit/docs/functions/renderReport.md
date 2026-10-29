# renderReport

Отрисовка режима report панели трекера (tmux-web/static/js/tracker/render-report.js).

Экран сводки за период: четыре карточки KPI (TOTAL, AVG/DAY, CLOSED, MAX FOCUS), столбики по дням, доли по спискам и по задачам, панель периода #trk-report-nav (day/week/month + [◀] [today] [▶]) и кнопка EXPORT CSV.

KPI считает СЕРВЕР (ReportKpi/build_kpi в plugins/tracker/src/report.rs) и присылает полем kpi ответа /api/tracker/report: каждое из чисел упирается либо в правило локальных суток, либо в правило идущего отрезка, и дублировать эти правила в JS означало бы разъезд с отчётом. Окно [from, to) полуоткрыто, поэтому число суток считается по дню момента to − 1 сек. Фильтр панели на сводку не влияет — иначе её сумма не сходилась бы с TODAY/WEEK статус-бара.
