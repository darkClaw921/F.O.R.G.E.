# renderTodoCard

Phase 4 — рендерит TODO-карточку (tmux-web/static/app.js). Создаёт div.kanban-card с data-status='todo' и data-priority. dragstart payload — 'todo:'+id (префикс маркирует TODO для drop-handler в renderColumn). click → openTodoEditModal(todo) — но не если кликнули по .promote-btn. Отрисовывает: title, description (truncate 140 char + …), meta-row (P-pill, type-tag, plan-mode-badge '◆ plan' если todo.plan_mode=true — forge-5bkg, ▲ promote button), labels (max 3 + +N). Кнопка promote вызывает promoteTodo(todo.id) с stopPropagation, чтобы не открывать edit-modal.
