# tmux-web/static/js/ws/attach.js

Phase 1. /ws/attach WebSocket: connectWs/disconnectWs/scheduleAttachWsReconnect/handleControlFromServer. Backoff [2s,4s,8s,16s,32s,60s] + jitter 1s. Перед connect делает fit() для корректных cols/rows. Закрытие через disconnectWs выставляет attachWsClosedByUs=true чтобы подавить reconnect. Бинарь PTY читается как Uint8Array в term.write.
