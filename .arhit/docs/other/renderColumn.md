# renderColumn

renderColumn(status, items) — рендер одной колонки kanban-board (tmux-web/static/app.js).

Возвращает <div.kanban-col data-status=...> с заголовком и body:
- .kanban-col-header: title (COLUMN_TITLES[status]) + .col-meta (.col-count + .col-add quick-create button; addBtn НЕ показывается для status='closed')
- .kanban-col-body[data-status=...]: контейнер карточек, рендер через renderCard для каждого issue из items

Phase 6.E (Drag-n-drop drop zone):
- dragover: preventDefault + dataTransfer.dropEffect='move' + .drop-target подсветка
- dragenter: preventDefault + .drop-target
- dragleave: убирает .drop-target ТОЛЬКО если курсор реально вышел (relatedTarget не contains в body — иначе drag над дочерним элементом ложно гасит подсветку)
- drop: preventDefault, читает dataTransfer.getData('text/plain') (issue.id), если targetStatus отличается от текущего issue.status — вызывает updateTask(id, {status: targetStatus}). updateTask делает optimistic apply и rollback при ошибке (Phase 6.C). Cross-tab sync — через WS upsert (Phase 6.D).

Quick-create '+' кнопка (Phase 6.C): открывает openCreateModal({status}) для preset колонки.

Зависимости: renderCard, openCreateModal, updateTask, COLUMN_TITLES, state.tasksData.
