# tmux-web/static/js/projects/projects.js

Phase 1. Multi-project: fetchProjects (GET /api/projects + restore projectFilter из localStorage forge.projectFilter; для активного проекта — re-open active TUI tab WS), renderProjectSelect (опция All projects + список), switchActiveProject (POST /api/projects/active + disconnectWs+fetchSessions+disconnectTasksWs/TodosWs+TUI switchCwd по newPath).
