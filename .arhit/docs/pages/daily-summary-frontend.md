Phase 4 фичи «Сводка дня»: отдельная фронтовая страница с рендером markdown.

Новые/изменённые файлы:
- tmux-web/static/js/core/markdown.js (НОВЫЙ): общий markdown-renderer renderMarkdownInto, вынесен из echo/chat.js. chat.js теперь импортирует его (import { renderMarkdownInto } from '../core/markdown.js').
- tmux-web/static/js/echo/api.js: добавлены listDailyReports(limit?), getDailyReport(day), generateDailyReport(day?) — обёртки над /api/echo/daily-reports* через call/jsonInit.
- tmux-web/static/index.html: контейнер #daily-summary (шапка с навигацией ←/Сегодня/→, кнопка пересоздания, область контента .echo-md, пустое состояние с кнопкой Сгенерировать).
- tmux-web/static/js/core/dom.js: 10 DOM-ссылок $dailySummary*.
- tmux-web/static/css/daily-summary.css (НОВЫЙ): стили вью по образцу home.css; подключён через @import в style.css.
- tmux-web/static/js/daily-summary/daily-summary.js (НОВЫЙ): showDailySummary(day?)/hideDailySummary().
- tmux-web/static/js/public-api.js: ForgeApp.showDailySummary / hideDailySummary.

Backend (Phase 1-3) предоставляет REST: GET /api/echo/daily-reports?limit=N, GET /api/echo/daily-reports/:day (404 если нет), POST /api/echo/daily-reports/generate {day?}.

XSS: весь markdown рендерится через createElement/textContent (core/markdown.js), пользовательские данные в innerHTML не попадают.