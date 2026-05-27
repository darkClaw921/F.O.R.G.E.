# saveDailyReportPrompts

API-клиент Echo (tmux-web/static/js/echo/api.js): PUT /api/echo/daily-reports/prompts через jsonInit('PUT', body). body = {report_prompt?, suggest_prompt?}; пустая строка в поле сбрасывает оверрайд к дефолту на бэкенде. Отвечает актуальным состоянием (как getDailyReportPrompts). Используется кнопками «Сохранить промпты» и «↺ дефолт» во вкладке настроек «Сводка дня».
