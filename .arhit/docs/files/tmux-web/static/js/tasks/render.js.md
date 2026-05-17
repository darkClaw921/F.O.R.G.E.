# tmux-web/static/js/tasks/render.js

Phase 1. Tasks kanban render: TASK_COLUMNS=[todo,open,in_progress,blocked,deferred,draft,closed], COLUMN_TITLES, CLOSED_LIMIT=20. renderTasks (board), compareIssues (priority asc, updated_at desc), renderColumn (DnD-aware: TODO принимает только в open; bd-issue запрещён в todo), renderTodoCard (drag payload 'todo:<id>'), renderCard. currentTodosProjectId возвращает state.activeProjectId.
