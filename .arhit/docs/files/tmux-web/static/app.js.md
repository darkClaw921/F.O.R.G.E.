# tmux-web/static/app.js

DEPRECATED. Phase 1 ES Modules refactor: весь код перенесён в /js/*. Файл оставлен пустым (только комментарий) для cache-warmth — старые открытые вкладки могут продолжать дёргать /app.js до hard-reload, пусть отдают пустой скрипт, а не 404. Главная точка входа теперь /js/main.js (type=module). Маппинг функций → новые модули см. в arhit doc show refactor-ui-modules.
