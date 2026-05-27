# generate

Handler POST /api/echo/daily-reports/generate в plugins/echo/src/routes/daily_reports.rs. Синхронно генерирует сводку дня через daily_report::generate_report (source=manual) с таймаутом GENERATE_TIMEOUT (90с). При успехе возвращает 200 с ПОЛНЫМ отчётом DailyReport через serde_json::to_value(report) — включая поле suggestions (массив предлагаемых задач по проектам); тот же формат, что и GET /daily-reports/:day. 400 на невалидный day, 504 при таймауте, 500 если отсутствует host adapter.
