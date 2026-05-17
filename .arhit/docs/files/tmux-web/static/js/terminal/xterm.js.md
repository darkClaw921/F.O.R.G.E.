# tmux-web/static/js/terminal/xterm.js

Phase 1 ES Modules. 1:1 копии initTerminal/sendResize/scheduleResizeFromTerm/setStatus/showPlaceholder из app.js. Импортирует state и DOM-refs ($terminalEl/$statusDot/$statusText/$placeholder). Подключает FitAddon + WebLinksAddon. Использует state.encoder для отправки PTY input через WebSocket. Сbрасывает lastResizeKey/triggers fit() при resize.
