# tmux-web/static/index.html

Phase 1 update: <script src=/app.js> заменён на <script type=module src=/js/main.js>. quick-cmd.js и hotkeys.js остались классическими (поскольку они IIFE-консумеры window.ForgeApp/QuickCmd/Hotkeys). Порядок в HTML: main.js → quick-cmd.js → hotkeys.js.

Аналитика содержит tablist «Агенты» / «Работа с Claude» и два независимых tabpanel hosts analytics-agents / analytics-claude. Старые summary/filter controls остаются в Claude panel; agent controls создаются DOM factories. Mode ARIA/data-mode синхронизируется analytics.js, общий back остаётся прежним. Прочие панели, пользовательские изменения и порядок загрузки скриптов сохраняются.
