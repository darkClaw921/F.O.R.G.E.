# tmux-web/static/js/settings/user-settings-api.js

Клиент для REST-эндпоинтов GET/PATCH /api/user-settings (реализованы в tmux-web/src/user_settings.rs + handlers в main.rs). Кэширует ответ в state.userSettings, реализует optimistic update + rollback.

# Экспорт

- fetchUserSettings() — GET /api/user-settings → state.userSettings = response; возвращает объект или null при ошибке.
  - При ошибке state.userSettings НЕ обнуляется (остаётся прежним значением). Если он был null (первая загрузка после сетевой ошибки) — останется null, и UI обязан использовать дефолты (см. инвариант 'нулевая конфигурация').
  - Используется в bootstrap.js (preload, best-effort, без await блокирующего стартап) и в settings/modal.js (refresh перед открытием TODO behavior tab).

- updateUserSettings(payload) — PATCH /api/user-settings с partial payload.
  - Optimistic update: state.userSettings = Object.assign({}, current, payload) ДО запроса. UI сразу видит новые значения.
  - На успехе: state.userSettings = ответ сервера (источник истины с возможным clamp priority).
  - На ошибке: rollback к prev (deep-cloned до мутации), throw Error для UI.
  - Используется в settings/todo-tab.js (Save handler).

# Контракт state.userSettings

- null — до первого успешного fetchUserSettings или после ошибки на старте.
- object — { todo_default_plan_mode, todo_default_priority, todo_default_issue_type, todo_plan_mode_suffix, todo_confirm_delete, todo_confirm_promote_on_drag }. Прочих полей нет, но клиент допускает forward-compat и при merge сохраняет неизвестные ключи через Object.assign.

UI код должен ВСЕГДА проверять state.userSettings на null/undefined и применять локальные дефолты — это критично для инварианта 'нулевая конфигурация = поведение как до фичи'.

# Optimistic update + rollback паттерн

1. prev = deepClone(state.userSettings) если не null, иначе null.
2. state.userSettings = merge(current, payload).
3. fetch PATCH.
4. r.ok → state.userSettings = response.json() (override merge, т.к. server мог clamp priority).
5. !r.ok или throw в fetch → state.userSettings = prev, throw Error(text || 'HTTP <status>').

deepClone() использует structuredClone при наличии, fallback к JSON.parse(JSON.stringify(...)) для совместимости со старыми браузерами (плоский объект — JSON-roundtrip достаточен).

# Связи

- state.js: импортирует state, мутирует state.userSettings.
- bootstrap.js: вызывает fetchUserSettings() при старте.
- settings/todo-tab.js: вызывает updateUserSettings(payload) в Save handler.
- settings/modal.js: может перед открытием вкладки делать fetchUserSettings() для актуализации.

Файл: tmux-web/static/js/settings/user-settings-api.js (~91 строк, 4KB).
