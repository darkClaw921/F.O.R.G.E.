# renderThemesPanel

Phase wk7.4: рендерер Themes-панели. Идемпотентен — безопасно вызывать многократно (например, после switchTheme).

Сигнатура: renderThemesPanel(panel: HTMLElement, themesState: { data: { presets, custom, active } })

Логика:
- Очищает panel.innerHTML и рисует две секции:

1. Секция 'Presets' — h3 .themes-section-title + .theme-card-grid с buildThemeCard на каждый theme из data.presets.
   Активная карточка (theme.id === data.active) → класс .active (border accent + box-shadow inset + badge).
   Click по карточке → switchTheme(theme.id) (Phase 3) → themesState.data.active = state.activeTheme.id → renderThemesPanel (без повторного GET).

2. Секция 'Custom themes' — header с h3 + кнопкой '+ New custom' (.theme-new-btn → openThemeEditor(null)).
   Сетка .theme-card-grid с custom-темами; пусто → .themes-empty 'No custom themes yet.'
   Каждая custom-карточка дополнительно содержит .theme-card-tools:
     - .theme-card-tool 'edit' → ev.stopPropagation + openThemeEditor(theme).
     - .theme-card-tool.theme-card-tool-danger 'del' → confirm + DELETE /api/themes/:id + reload.

Никогда не вызывает GET — реагирует только на локальный state. Для re-fetch нужно сбросить themesState.loaded и вызвать loadThemesIntoPanel.
