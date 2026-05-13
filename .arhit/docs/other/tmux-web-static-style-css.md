# tmux-web-static-style-css

tmux-web/static/style.css — стили для frontend tmux-web.

Структура (по секциям):
- Базовые стили layout (body, #app, sidebar, header)
- Sessions sidebar (.session-item, .btn-kill, attached-flag)
- Terminal frame (#terminal, #placeholder, status indicators)
- Kanban board (Phase 6.A): .kanban-col, .kanban-col-header, .kanban-col-body, .kanban-card с data-priority цветными border-left и .p-pill
- Modals (Phase 6.B/6.C): .modal-overlay, .modal-card, .task-modal, .modal-projects
- Project bar (Phase 6.B): #project-select, кнопки new/settings
- Phase 6.E (Drag-n-drop):
  - .kanban-card { cursor: grab } / .kanban-card:active { cursor: grabbing }
  - .kanban-card.dragging { opacity:0.5; cursor: grabbing } — визуально подсвечивает источник drag
  - .kanban-col-body.drop-target { outline:2px dashed #2a7fff; outline-offset:-4px; background: rgba(42,127,255,.05) } — целевая колонка во время dragover

Цветовая палитра: тёмный фон (#0d111c, #1d2230, #243046), акцент синий #2a7fff, P0 красный #e84d4d, P1 оранжевый #e89344.
