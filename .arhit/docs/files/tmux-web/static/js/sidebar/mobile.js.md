# tmux-web/static/js/sidebar/mobile.js

Phase 1. Sidebar collapse + mobile drawer: applySidebarCollapsed (desktop), setMobileSidebarOpen (drawer), toggleSidebar (диспетчер mobile/desktop), restoreSidebarState (read localStorage forge.sidebarCollapsed). После collapse — setTimeout 200ms для refit всех xterm/TUI.
