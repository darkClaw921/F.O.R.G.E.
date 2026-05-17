# tmux-web/static/js/themes/panel.js

Phase 1. Themes Settings panel: loadThemesIntoPanel (GET /api/themes, заполняет themesState), renderThemesPanel (Presets + Custom секции, активная карточка с .active рамкой), buildThemeCard (preview из 10 swatches: bg+fg+8 ANSI). Custom — кнопки edit/delete + + New custom (→openThemeEditor).
