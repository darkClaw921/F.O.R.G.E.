# plugins/tracker/src/report.rs

Чистые расчёты плагина forge-tracker: суммы, отчёт, KPI сводки и данные Гантта. Модуль не знает про стор и блокировки — все функции работают со срезами моделей и получают момент 'сейчас' явным параметром now, поэтому проверяемы точечными тестами.

ПРАВИЛА, КОТОРЫЕ ЖИВУТ ТОЛЬКО ЗДЕСЬ:
- сутки локальные; отрезок через локальную полночь режется по границе (split_by_local_day);
- отрезок с stopped_at = None считается идущим и учитывается до now;
- время пункта чек-листа — обычный TimeEntry с item_id и тем же task_id, поэтому во время задачи входит ровно один раз по построению.

КЛЮЧЕВЫЕ ТИПЫ: ReportGroupBy (day|task|list), ReportRow, Report {from,to,group_by,total_seconds,rows,kpi}, ReportKpi, GanttBar, GanttData, Totals.

ReportKpi — шапка экрана сводки, считается build_kpi(tasks, entries, from, to, now) и приезжает в ТОМ ЖЕ ответе, что строки (KPI от оси группировки не зависят):
- total_seconds — сумма окна;
- days — календарных локальных суток в окне (окно полуоткрытое: [08.09, 09.09) это одни сутки); минимум 1, чтобы среднее не делилось на ноль; у открытого окна вместо календарных берутся дни с активностью;
- avg_day_seconds = total/days;
- prev_total_seconds — сумма за предыдущее окно той же длины (подпись '+3:12 к прошлой'); None у открытого окна;
- closed_tasks / touched_tasks — закрытых в окне (done_at внутри) из числа затронутых (было время в окне либо закрыта в окне); знаменатель НЕ 'все задачи стора' намеренно;
- max_focus_seconds / max_focus_task_id / max_focus_day — самый длинный ОДИН отрезок, обрезанный границами окна, и локальный день его начала.

ОСНОВНЫЕ ФУНКЦИИ: parse_ts (RFC 3339 или голая дата как локальная полночь), local_day_start, entry_span, entry_seconds_in_window, split_by_local_day, task_seconds, item_seconds, build_report, build_kpi, build_gantt, build_totals.

ПОТРЕБИТЕЛИ: store.rs (report_at/gantt/totals — тонкие обёртки над read()), routes/report.rs (GET /api/tracker/report и /gantt), фронт tmux-web/static/js/tracker/render-report.js и render-gantt.js (только показывают посчитанное).
