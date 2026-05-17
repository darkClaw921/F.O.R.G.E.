# tmux-web/static/js/echo/autonomous.js

Echo autonomous tasks UI. initAutonomousPane — вешает + New button. refreshAutonomous — список из listAutonomousTasks, рендерит с toggle/Run/Runs/Edit/Delete buttons. openCreateModal/openEditModal — overlay с form (name, prompt_template, interval_seconds, model, project_id). openRunsModal — table с status/tokens/error из listAutonomousRuns. INTERVAL_PRESETS: 1m/5m/15m/1h/6h/24h. Все ошибки через notify.
