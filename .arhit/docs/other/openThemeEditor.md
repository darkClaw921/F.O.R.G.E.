# openThemeEditor

Phase 5 (forge-wk7.5): полнофункциональный редактор кастомных тем.

Сигнатура: openThemeEditor(themeOrNull: Theme | null) → void

Режимы:
- create: themeOrNull === null. Заголовок 'New custom theme', baseline draft — клон активной темы (state.activeTheme) либо первого пресета, либо нули. Имя пустое.
- edit: themeOrNull — объект Theme с id, существующий в custom. Заголовок 'Edit theme: {name}', поля заполнены из объекта.

Структура DOM:
- .modal-overlay (через buildModalOverlay) + .modal-card.theme-editor-modal (720px)
- .theme-editor-header: h2 с заголовком + .theme-editor-close (×)
- .theme-editor-body:
  - .theme-editor-meta (grid 1:1): name input + duplicate-from-preset select
  - section UI colors → .theme-editor-ui-grid (2col, 11 пикеров через buildColorPickerRow)
  - section Terminal colors → .theme-editor-term-base-grid (2col, 4 base) + .theme-editor-ansi-grid (4×4 для 16 ANSI)
  - section Live preview → buildLivePreviewContainer().el
- .modal-actions.theme-editor-actions: Cancel + Save (primary)

Локальный state:
- draft: { id, name, ui: {11 полей}, term: {20 полей} } — все hex.
- presets[]: загружается асинхронно через GET /api/themes для dropdown.
- uiRefs/termRefs: { [key]: { el, setValue(hex) } } — DOM-ссылки пикеров для applyPresetToDraft без re-render.

Поведение:
- Изменение любого пикера → мутация draft + updatePreview().
- Duplicate-from-preset change → applyPresetToDraft(preset): копирует 31 значение, если name пуст — подставляет 'Copy of {presetName}'. Select сбрасывается на 'From scratch'.
- Save: validateDraft → buildThemePayload → POST/PUT /api/themes/custom[/id]. При 2xx — close + loadThemesIntoPanel(#ps-panel-themes). При 4xx — alert.
- Cancel/X/click-outside → overlay.remove() без сохранения.

Зависимости: buildColorPickerRow, buildLivePreviewContainer, validateDraft, buildThemePayload, normalizeHex, cloneThemeColors, THEME_UI_KEYS, THEME_TERM_BASE_KEYS, THEME_TERM_ANSI_KEYS, THEME_TERM_KEYS, HEX_COLOR_RE, buildModalOverlay, loadThemesIntoPanel, state.activeTheme.

Используется из renderThemesPanel: '+ New custom' → openThemeEditor(null); edit-кнопка custom-карточки → openThemeEditor(theme).
