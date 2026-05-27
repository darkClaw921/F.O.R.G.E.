# getDailyReportPrompts

API-клиент Echo (tmux-web/static/js/echo/api.js): GET /api/echo/daily-reports/prompts. Возвращает {report_prompt, suggest_prompt, report_prompt_default, suggest_prompt_default} — эффективные кастомные промпты генерации сводки дня (report) и предложений задач (suggest) вместе с встроенными дефолтами. Тонкий wrapper над call(). Используется renderDailySummaryTab для заполнения textarea при рендере вкладки «Сводка дня».
