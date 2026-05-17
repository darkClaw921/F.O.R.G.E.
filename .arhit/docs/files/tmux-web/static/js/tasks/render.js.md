# tmux-web/static/js/tasks/render.js

Phase 1. Tasks kanban render: TASK_COLUMNS=[todo,open,in_progress,blocked,deferred,draft,closed], COLUMN_TITLES. renderTasks (board), compareIssues (priority asc, updated_at desc), renderColumn (DnD-aware: TODO принимает только в open; bd-issue запрещён в todo), renderTodoCard (drag payload 'todo:<id>'), renderCard. currentTodosProjectId возвращает state.activeProjectId. ИЗМЕНЕНИЕ (forge-ja22): убран CLOSED_LIMIT=20 — колонка closed теперь показывает все закрытые задачи (backend tasks.rs уже отдаёт --limit 0, ограничение было только на UI).
