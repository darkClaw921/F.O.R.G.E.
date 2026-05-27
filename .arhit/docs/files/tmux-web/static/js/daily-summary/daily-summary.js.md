# tmux-web/static/js/daily-summary/daily-summary.js

Фронтенд страницы «Сводка дня» (tmux-web, vanilla JS module). showDailySummary() — открывает панель #daily-summary, грузит список отчётов через echo API (GET /api/echo/daily-reports), рендерит markdown выбранного дня через core/markdown.renderMarkdownInto, поддерживает навигацию по датам (предыдущий/следующий день) и кнопку перегенерации (POST /api/echo/daily-reports/generate с текущим day). Экспонируется через ForgeApp.
