# modal.js

ES-module Settings modal (tmux-web/static/js/settings/modal.js). Экспортирует openSettingsModal(initialTab) — открывает оверлей с табами настроек.

Табы:
- Notifications (default) — список проектов с раскрываемыми формами уведомлений (buildNotificationsForm из notifications-tab.js).
- Themes — lazy-load через loadThemesIntoPanel при первом клике (themesState.loaded флаг).
- TODO behavior (Phase 2) — lazy renderTodoPanel при первом клике: если state.userSettings===null делает fetchUserSettings() (best-effort), затем buildTodoBehaviorForm(state.userSettings || {}, onSaved). onSaved обновляет state.userSettings ответом сервера. todoState.loaded флаг.
- Remote servers — виден только в isRemoteMode(); renderRemotesTable + форма Add new server (label/URL/token, Test connection, Save).

showTab(name): переключает .modal-tab-btn.active + panel.hidden, диспатчит lazy-loaders.

initialTab: поддерживает 'themes', 'todo', 'remotes' (только если remote-mode).

Зависимости: state, buildModalOverlay (utils), isRemoteMode/fetchRemoteServers (remote), fetchProjects (projects), renderSidebar (sidebar), loadThemesIntoPanel (themes/panel), buildNotificationsForm (notifications-tab), renderRemotesTable (remotes-tab), buildTodoBehaviorForm + fetchUserSettings (Phase 2).
