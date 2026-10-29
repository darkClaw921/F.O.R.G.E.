# renderGantt

renderGantt(container, view) — отрисовка режима gantt панели forge-tracker (tmux-web/static/js/tracker/render-gantt.js). Вызывается из renderGanttMode() в js/tracker/main.js, когда local.mode === 'gantt'.

Строит ОДНУ CSS-сетку .trk-g-grid; строки объявлены display: contents и становятся ячейками этой сетки, поэтому колонки est и факт не разъезжаются между строками. Порядок содержимого: шапка (// NODE, номера дней, est, факт) → дорожка // MILESTONES и полоса чипов с подписями вех → заголовки групп-списков (кнопки сворачивания) → строки задач → легенда вне области прокрутки.

Поля view: bars (полосы окна, уже пересечённые с выборкой панели), milestones, days (ось из buildDays), tasksById, lists, taskSeconds, runningIds, collapsed, now, onOpen, onToggleGroup, onAddMilestone, onEditMilestone.

Строка задачи: приоритет из задачи снимка (в полосе Гантта его нет), заголовок через textContent, символьная дорожка из buildTrack, колонка est (~4ч) и колонка факта. Колонка факта несёт data-trk-task-time — тот же атрибут, что в ленте и на доске, поэтому её обновляет общий тикер панели, а своего таймера Гантту не нужно. Идущая задача получает класс running (акцентная полоса слева и зелёное время), превысившая оценку — класс over. Задача без start и без due и без срока в окне показывает «— без плана —» вместо строки из пробелов.
