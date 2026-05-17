# tmux-web/static/js/echo/model-picker.js

Echo model picker. initModelPicker(onChange) — заполняет <select id='echo-model-picker'> и навешивает change-handler. Список DEFAULT_MODELS hardcoded (claude-opus-4-5, claude-3-5-sonnet-latest, claude-3-5-haiku-latest). getSelectedModel() читает из localStorage key 'forge.echo.model', fallback на первую модель. listModels() — копия списка для других UI.
