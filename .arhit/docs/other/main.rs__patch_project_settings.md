# main.rs::patch_project_settings

Phase 3 — PATCH /api/projects/:id/settings: обновляет notify-настройки проекта. Body: notify_template?, notify_delay_minutes?, notify_wait_previous?, notify_session? (все опциональны, отсутствие=не трогать, null для notify_session=стереть). Логика: ProjectStore::update_settings → 404 если нет проекта → atomic save → 200 ProjectDto.
