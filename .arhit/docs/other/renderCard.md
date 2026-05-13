# renderCard

renderCard(issue) — фабрика DOM-элемента карточки в kanban-board (tmux-web/static/app.js).

Возвращает <div.kanban-card> с data-id, data-status, data-priority. Структура:
- .id (id задачи, mono)
- .title (текст)
- .meta-row: .p-pill (P0..P4) + .type-tag (issue_type)
- .labels: до 3 .label-tag + +N если больше

Phase 6.C: addEventListener('click') открывает edit-modal через openEditModal(issue).

Phase 6.E (Drag-n-drop):
- card.draggable = true
- dragstart: setData('text/plain', issue.id), effectAllowed='move', добавляет .dragging
- dragend: убирает .dragging, чистит висящие .drop-target подсветки
- dragMoved-флаг подавляет click сразу после drag-end (через setTimeout 0), чтобы отпускание мыши не открывало edit-modal

Зависимости: openEditModal (Phase 6.C), updateTask (через drop в renderColumn body).
