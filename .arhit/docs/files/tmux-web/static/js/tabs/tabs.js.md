# tmux-web/static/js/tabs/tabs.js

Tab dispatcher. switchTab(name) переключает active tab между terminal/tasks/git/docker/telescope/echo. Управляет .hidden и .active классами, лайфциклом WS соединений: tasks→connectTasksWs/stopTasksPolling, git/docker/telescope→TUI terms, echo→initEcho+connectEchoWs/disconnectEchoWs. Phase 5c добавлен 'echo' и lifecycle import из echo/main.js.
