# tmux-web/static/js/core/bootstrap.js

Bootstrap entry. Импортирует все feature-модули, монтирует click-handlers на tabs (terminal/tasks/git/docker/telescope/echo), стартует polling/WS соединений, обрабатывает beforeunload (cleanup all) и visibilitychange (pause/resume). Phase 5c добавлен Echo: import initEcho/connectEchoWs/disconnectEchoWs/teardownEcho из echo/main.js, eager initEcho() для preload chats, teardownEcho в beforeunload, disconnect/reconnect Echo в visibilitychange.
