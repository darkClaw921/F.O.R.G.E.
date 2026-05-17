# tmux-web/static/js/settings/remotes-tab.js

Phase 1. renderRemotesTable($tbody) — таблица remoteServers с edit/delete. openEditRemoteRow(tr, srv, $tbody) — inline form (Label + опциональный новый token). PATCH/DELETE /api/remote-servers/:id, после успеха — fetchRemoteServers + renderRemotesTable + renderSidebar.
