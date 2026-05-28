# gantt.js

Гант-таймлайн задач вкладки Tasks (tmux-web/static/js/tasks/gantt.js). Рендерит ось дней, строки задач (in_progress/closed) и вертикальные черты git-коммитов. Состояние: state.tasksData.issues, state.gitCommits, state.ganttRange.

ГРУППИРОВКА ПО КОММИТАМ (groupByCommit): видимые задачи группируются по 'закрывающему' коммиту — группа задачи = первый коммит с ts>=anchor (anchor=closed_at для closed, иначе t1). Задачи без последующего коммита уходят в хвостовую группу OPEN_GROUP_KEY ('Без коммита / в работе'). Группы сортируются по ts коммита; каждая несёт gStart=min(start), gEnd=max(end|t1), totalMs, hasOngoing. commit.ts в секундах → *1000.

СВЁРНУТОСТЬ: модульный Set expandedGroups (ключи = hash коммита / OPEN_GROUP_KEY). По умолчанию пуст → все группы свёрнуты. Клик по .gantt-group-label toggle'ит ключ и вызывает renderGantt() (перерисовка из state).

РЕНДЕР: renderGroupHeader рисует строку .gantt-group (каретка ▶/▼, метка subject коммита, summary-бар .gantt-group-bar [gStart,gEnd], бейдж .gantt-group-duration с fmtDuration(totalMs)). Развёрнутая группа дорисовывает renderTaskRow(...,grouped=true) на каждую задачу.

ПОПАП РАЗБИВКИ: attachGroupHover на бейдже длительности → renderGroupPopover(group) переиспользует shared-попап (ensurePopover/positionPopover/scheduleHide/showTimer/hideTimer, общий с попапом коммита). Показывает шапку (subject + общая длительность + диапазон + кол-во задач) и список задач: id, title, полное description (≤800 симв.), длительность каждой (fmtDuration(end-start) или 'в работе'). Всё через textContent.

ХЕЛПЕРЫ: fmtDuration(ms) — человекочитаемая длительность ('3д 4ч','2ч 15м','45м','30с', макс 2 старшие единицы). Существующие: parseTs, clamp, fmtDate, fmtAxisDate/Time, shortTitle, buildAxis, ganttWindow.

Экспорты: ganttWindow, renderGantt, fetchGitCommits, initGanttControls. CSS — раздел Gantt timeline в tmux-web/static/css/tasks.css (.gantt-group*, .gantt-group-tasks, попап переиспользует .gantt-commit-popover).
