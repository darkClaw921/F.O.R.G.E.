# $tasksGantt

DOM-ссылка на контейнер #tasks-gantt — блок гант-диаграммы под канбан-доской вкладки Tasks (tmux-web/static/index.html). Объявлена в tmux-web/static/js/core/dom.js рядом с $tasksBoard. Содержит #gantt-toolbar (заголовок Timeline + переключатель диапазона #gantt-range с кнопками 7д/30д/Всё) и #gantt-canvas. Phase 2 (forge-zssi) — каркас без логики рендера; рендер добавляется в Phase 3 (gantt.js).
