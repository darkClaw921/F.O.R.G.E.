# tmux-web/static/js/tabs/tui-tabs.js

Phase 1. TUI tabs framework: createTuiTab factory (lazygit/lazydocker/telescope), initTuiTabs (создаёт 3 TuiTab и кладёт в state.gitTerm/dockerTerm/telescopeTerm), getActiveProject, мин-aliases mountGitTerm/openLazygitForActiveProject/connectGitWs/closeGitWs/gitSwitchCwd/showGitBanner/hideGitBanner/retryGitConnection. sendToActivePty(text) — диспетчер по state.activeTab для quick-cmd.js. LAZYGIT/LAZYDOCKER/TELESCOPE_INSTALL_ENTRIES — таблицы команд установки для banner help. Channel-bar для telescope (Files/Content/Dirs/GitLog).
