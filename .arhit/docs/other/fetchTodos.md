# fetchTodos

Phase 4 — асинхронная функция в tmux-web/static/app.js. GET /api/todos?project_id=<pid> (или без параметра — бэкенд возьмёт активный проект). При успехе кладёт массив в state.todosData и вызывает renderTasks(). При сетевой/HTTP-ошибке логирует warn и кладёт пустой массив, чтобы board остался отрисованным. projectId = currentTodosProjectId() = state.activeProjectId. Вызывается из bootstrap (после fetchProjects), из visibilitychange (если WS не OPEN), из switchActiveProject, и как fallback poll каждые 30s через startTodosPolling.
