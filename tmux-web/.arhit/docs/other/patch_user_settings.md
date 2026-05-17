# patch_user_settings

REST-handler PATCH /api/user-settings в tmux-web/src/main.rs.

Принимает JSON-тело типа PatchUserSettingsReq (см. user_settings.rs) — все поля Option<T>. Применяются только Some-варианты через state.user_settings.patch(payload).

Валидация:
- todo_default_priority клампится в 0..=4 в Store (>4 → 4)
- todo_plan_mode_suffix принимается без trim — клиент сам решает что хранить

При успехе → 200 + JSON c обновлённым UserSettings.
При ошибке IO → 500 + сообщение.

Логирование: info при успехе, error с деталями при провале.
