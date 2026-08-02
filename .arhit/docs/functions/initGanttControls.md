# initGanttControls

initGanttControls (tmux-web/static/js/tasks/gantt.js) — инициализация управления блоком Timeline на вкладке Tasks.

Делает две вещи:
1. initGanttToggle() — вешает клик на кнопку #gantt-toggle (заголовок 'Timeline'), которая переключает класс .collapsed на контейнере #tasks-gantt. В свёрнутом состоянии CSS скрывает #gantt-canvas (display:none) и убирает фиксированную высоту 34%, оставляя только тулбар. По умолчанию блок СВЁРНУТ — класс .collapsed проставлен прямо в static/index.html; состояние не персистится, каждая загрузка страницы начинается со свёрнутого Timeline. При разворачивании вызывается renderGantt(), т.к. данные могли обновиться пока блок был скрыт. Также обновляется aria-expanded, каретка ▸ поворачивается на 90° через CSS.
2. Вешает клики на кнопки диапазона #gantt-range (today/yesterday/7/30/all) → state.ganttRange + fetchGitCommits().

Обе части идемпотентны: guard через dataset-флаги (ganttToggleBound на кнопке, ganttBound на контейнере диапазонов).
