# initGanttControls

Навешивает обработчики на кнопки переключателя диапазона #gantt-range (tmux-web/static/js/tasks/gantt.js, exported). Идемпотентно: guard через root.dataset.ganttBound. Кнопки button[data-range] (today|yesterday|7|30|all). По клику: state.ganttRange = (data-range==='all'||'today'||'yesterday') ? строка как есть : Number(data-range) (число дней); переключение класса .active на нажатой кнопке; затем fetchGitCommits() (since/until изменились → перезагрузка коммитов + рендер). Поддержка именованных диапазонов 'today'/'yesterday' добавлена в Phase B вместе с until-параметром.
