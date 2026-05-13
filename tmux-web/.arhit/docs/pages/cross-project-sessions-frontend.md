## Cross-project sessions visibility — Frontend (Phase 2)

### State
- state.projectFilter: '__all__' | <project.id> — UI-фильтр сайдбара. Persist в localStorage('forge.projectFilter').
- state.activeProjectId — backend-side активный проект, остаётся НЕ затронутым фильтром.

### Поведение
- При загрузке fetchProjects читает localStorage; если значение валидно (=== '__all__' либо matches state.projects[].id) — восстанавливает, иначе fallback '__all__'.
- renderProjectSelect: первая опция всегда 'All projects' (value='__all__'), selected = state.projectFilter.
- change на #project-select: НЕ дёргает switchActiveProject/fetchSessions/disconnectWs. Только обновляет state.projectFilter, localStorage и вызывает renderSidebar().
- renderSidebar:
  * '__all__' режим: группы по project_id в порядке state.projects, заголовок <li class='session-group-header'>name</li> для каждой непустой группы; orphan (project_id===null) — последний с заголовком 'Orphan'.
  * single-project режим: плоский список без header; empty-state 'Нет сессий в этом проекте' если у выбранного проекта нет сессий.

### Стили
.session-group-header: padding 6px 12px 4px, font-size 11px, uppercase, letter-spacing 0.5px, color #6c7587, background #181c25, border-top 1px #232936, cursor default, list-style none. :first-child без верхней границы.

### Зависимости
- Phase 1: SessionDto.project_id/project_name (Option) + GET /api/sessions без фильтра.
- attention-watcher поллит все сессии — нужно для корректного .needs-attention в orphan-группе.