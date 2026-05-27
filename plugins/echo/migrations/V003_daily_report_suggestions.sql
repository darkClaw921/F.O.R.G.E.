-- V003_daily_report_suggestions.sql — предлагаемые задачи по проектам для «Сводки дня».
--
-- Добавляет колонку suggestions к daily_reports: JSON-массив (как TEXT)
-- предложенных задач, сгруппированных по проектам. Структура значения:
--   [{"project_path":"...","project_name":"...",
--     "tasks":[{"title":"...","description":"...","priority":2}]}]
--
-- Notes:
--   * nullable — старые записи и пустые дни хранят NULL/"[]"; repo парсит
--     NULL/невалидный JSON как пустой массив.
--   * rust-embed подхватит файл по имени, трекинг в schema_migrations.

ALTER TABLE daily_reports ADD COLUMN suggestions TEXT;
