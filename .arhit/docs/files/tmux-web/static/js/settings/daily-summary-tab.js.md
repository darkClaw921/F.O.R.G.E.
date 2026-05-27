# tmux-web/static/js/settings/daily-summary-tab.js

Вкладка «Сводка дня» в settings-модалке (tmux-web). renderDailySummaryTab(panel, {onClose}) — рендерит кнопки «Сгенерировать сейчас» (POST /api/echo/daily-reports/generate) и «Открыть страницу» (вызывает showDailySummary, закрывая модалку через onClose). Ленивый рендер с флагом loaded. initialTab='daily-summary' открывает эту вкладку напрямую.
