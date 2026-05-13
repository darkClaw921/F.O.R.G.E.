# projects.rs::ProjectStore::update_settings

Phase 3 — Обновляет notify-настройки проекта. Параметры: notify_template (Option<String>), notify_delay_minutes (Option<u32>), notify_wait_previous (Option<bool>), notify_session (Option<Option<String>> для различения Some(None)=стереть и None=не трогать). Возвращает обновлённый клон Project или None если id не найден. НЕ сохраняет на диск автоматически — caller вызывает save() явно.
