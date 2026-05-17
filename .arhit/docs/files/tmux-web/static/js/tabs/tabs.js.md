# tmux-web/static/js/tabs/tabs.js

Phase 1. switchTab(name) — главный диспетчер: terminal/tasks/git/docker/telescope. Toggle hidden на контейнерах, .active на кнопках. При уходе с tabs/git/docker/telescope закрывает WS. При приходе на tasks — connectTasksWs+(fetchTasks if no data) или renderTasks.
