# tmux-web/static/js/themes/editor.js

Phase 1. openThemeEditor(themeOrNull) — модал редактора кастомных тем. Поля: name, Duplicate from preset dropdown (lazy GET /api/themes), UI grid (11 пикеров) + Terminal base grid (4) + ANSI grid (16) через buildColorPickerRow. Live preview через buildLivePreviewContainer (мини-UI + 16 ANSI span'ов). Save → POST /api/themes/custom (create) или PUT /api/themes/custom/:id (edit) + перерисовка panel.
