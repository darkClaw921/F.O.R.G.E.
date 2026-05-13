# renderSidebar

Перерисовывает сайдбар сессий в static/app.js. Cross-project sessions visibility (Phase 2): группировка по project_id в зависимости от state.projectFilter.

Режимы:
- state.projectFilter === '__all__' (default): все сессии группируются по проектам. Порядок групп — как в state.projects (то есть как backend вернул в /api/projects); orphan-группа (project_id === null) идёт последней с заголовком 'Orphan'. Каждая непустая группа предваряется заголовком <li class='session-group-header'>{project.name}</li>. Если ни одной сессии нет — empty-state 'Нет активных сессий'.
- state.projectFilter === <project.id>: только сессии этого проекта, плоский список без header. Если у выбранного проекта нет сессий — empty-state 'Нет сессий в этом проекте'.

Сортировка внутри группы — по name.localeCompare. Для рендера каждой строки делегирует в buildSessionItem (вынесена ради переиспользования между обоими режимами).

Триггеры renderSidebar:
- fetchSessions (раз в 3с polling).
- change на #project-select (handler обновляет state.projectFilter и localStorage, дёргает только renderSidebar — БЕЗ switchActiveProject/disconnectWs/fetchSessions, ничего серверного не происходит).

Зависит от:
- state.sessions (массив SessionDto с project_id/project_name).
- state.projects (порядок и имена групп).
- state.projectFilter (режим).
- state.currentSession (для .active в buildSessionItem).
- buildSessionItem (рендер строки).

Файл: static/app.js.
