# app.js::renderSidebar

Legacy local-mode рендер sidebar (tmux-web/static/app.js, ~446–542). После Phase 2 (forge-fd5d) группировка переключена на folder_id вместо project_id.

Поведение:
- Если isRemoteMode() === true — делегирует renderSidebarWithOrigin() и выходит (remote-ветка не затронута).
- Если state.sessions пуст — рендерит li.empty 'Нет активных сессий'.
- projectFilter применяется ДО группировки: visible = sessions.filter(s.project_id === filter) при filter !== '__all__'. Если visible пуст — 'Нет сессий в этом проекте' (или 'Нет активных сессий' в __all__).
- Группирует visible по folder_id (orphan = null). Внутри каждой группы сессии сортируются по name.localeCompare().
- Сортировка ключей групп: по folder_label (case-insensitive localeCompare), ORPHAN_KEY всегда в конце.
- Header группы: arr[0].folder_label || keyDisplay, где keyDisplay = key.startsWith('__folder:') ? key.slice('__folder:'.length) : key.

Ключевые контракты:
- folder_id / folder_label берутся из SessionDto (Phase 1, форматы '__folder:<full_path>' и basename соответственно).
- project_id остаётся в DTO и используется (а) фильтром project-bar (state.projectFilter), (б) openSession() → targetProjectId = sess.project_id → switchActiveProject. В sidebar group-header больше не зависит от state.projects.
- Удалён старый блок 'for (const p of state.projects) { ... }' и авто-группа по project_id вида '__path__:...': теперь единый проход по Map<folder_id, Session[]>.

Зависимости:
- buildSessionItem(sess) для рендера каждого элемента.
- isRemoteMode(), state.projectFilter, state.sessions.

Связанные задачи: forge-fd5d (P2.1), forge-a15o (Phase 2 epic). Phase 3 (forge-1log) переименует groupSessionsByProject → groupSessionsByFolder в remote-ветке (НЕ затронуто здесь).
