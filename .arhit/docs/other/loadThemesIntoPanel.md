# loadThemesIntoPanel

Phase wk7.4: загрузчик Themes-панели в settings-modal.

Сигнатура: loadThemesIntoPanel(panel: HTMLElement, themesState: { loaded, data })

Шаги:
1. Показывает .themes-loading placeholder в panel.
2. GET /api/themes → ожидает { presets: Theme[], custom: Theme[], active: string }.
3. Нормализует shape (массивы по умолчанию пустые, active — строка|null) и сохраняет в themesState.data, выставляет themesState.loaded=true.
4. Зовёт renderThemesPanel(panel, themesState).
5. На любую ошибку (сеть, не-2xx, парсинг) — рисует .themes-error + кнопку .themes-retry, которая повторно вызывает loadThemesIntoPanel.

Используется в openSettingsModal: первый показ Themes-вкладки → загрузка; повторные переключения вкладок не дёргают GET (флаг themesState.loaded). После DELETE custom-темы кэш сбрасывается (themesState.loaded=false) и идёт reload.
