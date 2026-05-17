# tmux-web/static/index.html

Phase 1 update: <script src=/app.js> заменён на <script type=module src=/js/main.js>. quick-cmd.js и hotkeys.js остались классическими (поскольку они IIFE-консумеры window.ForgeApp/QuickCmd/Hotkeys). Порядок в HTML: main.js → quick-cmd.js → hotkeys.js.
