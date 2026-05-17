# tmux-web/static/js/public-api.js

Phase 1. Контракт window.ForgeApp = { sendToActivePty, state }. Импортирует sendToActivePty из tabs/tui-tabs.js и state из core/state.js. ВАЖНО: на этот контракт завязан quick-cmd.js (217-221, 585-591) — нельзя менять без обновления quick-cmd.js.
