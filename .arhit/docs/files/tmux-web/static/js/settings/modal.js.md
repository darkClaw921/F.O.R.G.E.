# tmux-web/static/js/settings/modal.js

Phase 1. openSettingsModal (initialTab?). Tabs: Notifications/Themes/[Remote servers — только в remote mode]. Notifications-таб — список проектов с раскрывающейся формой buildNotificationsForm. Remotes-таб — таблица renderRemotesTable + форма Add (Label/URL/Token, Test connection через POST /api/remote-servers + GET /healthz, Save без preflight). themesState lazy-load через loadThemesIntoPanel.
