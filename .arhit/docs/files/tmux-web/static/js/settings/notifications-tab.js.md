# tmux-web/static/js/settings/notifications-tab.js

Phase 1. buildNotificationsForm(project, onSaved) — fieldset с template/delay_minutes/wait_previous/session_override. saveProjectSettings(id, payload) — PATCH /api/projects/:id/settings с optimistic apply на state.projects + rollback при ошибке. Возвращает {ok, project} либо {ok:false, error}.
