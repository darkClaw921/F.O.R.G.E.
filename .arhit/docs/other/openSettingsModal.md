# openSettingsModal

Phase wk7.4 (themes UI): settings-modal теперь содержит tab-bar с двумя вкладками — Notifications (исторический контент: список проектов + per-project notify-форма) и Themes (новая, Phase wk7).

Структура DOM:
- .modal-card.settings-modal
  - .modal-tabs (role=tablist)
    - .modal-tab-btn[data-tab=notifications].active (по умолчанию)
    - .modal-tab-btn[data-tab=themes]
  - .modal-tab-panel[data-panel=notifications] — h2 + ul#ps-list (renderList: проекты + buildNotificationsForm)
  - .modal-tab-panel[data-panel=themes][hidden] — h2 + .themes-content (Phase 4 рендер)
  - .modal-actions — кнопка Close

showTab(name) переключает active-класс tab-btn и hidden у panels. При первом открытии Themes-вкладки вызывает loadThemesIntoPanel + кэш themesState.

Сохранён ВЕСЬ функционал Notifications-вкладки: expanded Set, renderList, buildNotificationsForm, optimistic-обновление через saveProjectSettings — без изменений.
