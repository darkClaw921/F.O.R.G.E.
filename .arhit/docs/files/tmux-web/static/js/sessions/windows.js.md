# tmux-web/static/js/sessions/windows.js

Phase 1. Windows of active tmux session: fetchWindows, renderWindowBar, selectWindow, createWindow, killWindow, renameWindow, startWindowsPolling(2s)/stopWindowsPolling. 400/404 ответы очищают currentWindows. Все REST через apiFetch с state.attachWsOrigin.
