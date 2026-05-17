# tmux-web/static/js/sidebar/origin-tabs.js

Phase 1. Origin tabs + persistence: renderOriginTabs (All/Local/+remotes/+plus), loadActiveOriginFromStorage, saveActiveOriginToStorage, _collapsedOrigins Set + get/persist/is/toggle. localStorage keys: forge.activeOrigin, forge.collapsedOrigins. Клик по табу — lazy-load remoteSessions/Projects + переподключение tasks/todos WS.
