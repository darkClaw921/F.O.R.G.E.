# tmux-web/static/js/tabs/tui-tabs.js

Phase 1+. TUI tabs framework: createTuiTab factory (lazygit/lazydocker/telescope), initTuiTabs (создаёт 3 TuiTab и кладёт в state.gitTerm/dockerTerm/telescopeTerm), getActiveProject, мин-aliases mountGitTerm/openLazygitForActiveProject/connectGitWs/closeGitWs/gitSwitchCwd/showGitBanner/hideGitBanner/retryGitConnection. sendToActivePty(text) — диспетчер по state.activeTab для quick-cmd.js. LAZYGIT/LAZYDOCKER/TELESCOPE_INSTALL_ENTRIES — таблицы команд установки для banner help. Channel-bar для telescope (Files/Content/Dirs/GitLog).

## resolveCwd и session-sync

Каждой TuiTab можно передать opts.resolveCwd — функцию, возвращающую cwd при openForActiveProject. Используется для привязки к cwd текущей tmux-сессии вместо корня проекта.

Сейчас resolveCwd задан для:
- gitTerm (lazygit): sessionCwdOrNull() — lazygit показывает репо именно той сессии, в которой работает юзер.
- telescopeTerm (Find): sessionCwdOrNull() — fuzzy-finder ищет в каталоге текущей сессии.

Fallback на project.path выполняется внутри openForActiveProject, если сессия не выбрана.

## Sync-функции

- syncGitToCurrentSession() — если gitTerm.ws открыт и cwd сессии отличается от текущего, делает switchCwd (бэк перезапускает lazygit под новым cwd).
- syncTelescopeToCurrentSession() — аналогично для telescope (tv перезапускается).

Обе вызываются из sessions.js (openSession/switchSession) и из tasks-ws.js рядом с syncTasksToCurrentSession().
