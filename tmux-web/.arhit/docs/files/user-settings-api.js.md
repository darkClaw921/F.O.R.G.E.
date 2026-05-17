# user-settings-api.js

ES-module клиент для пользовательских настроек поведения TODO (Phase 2 feature). Путь: tmux-web/static/js/settings/user-settings-api.js.

Экспортирует:
- fetchUserSettings(): GET /api/user-settings, кеширует ответ в state.userSettings. Возвращает settings-объект или null при ошибке (404/500/network). state.userSettings не сбрасывается при ошибке — остаётся прежним значением (null до первого успешного fetch).
- updateUserSettings(payload): PATCH /api/user-settings с optimistic update — сразу мержит payload в state.userSettings, затем шлёт запрос. При HTTP-ошибке или исключении делает rollback к prev (deep-clone снимок до изменений) и пробрасывает Error для UI. На успехе state.userSettings = ответ сервера.

Внутренний helper deepClone использует structuredClone при доступности, fallback на JSON.parse(JSON.stringify) для совместимости.

Зависимости: state из ../core/state.js (читает/пишет userSettings).
Использование: bootstrap.js (preload best-effort), settings/modal.js (lazy reload при открытии вкладки), settings/todo-tab.js (Save button).
