Фича forge-meoa Phase 1: предлагаемые задачи по проектам в «Сводке дня».

Миграция V003_daily_report_suggestions.sql: ALTER TABLE daily_reports ADD COLUMN suggestions TEXT (nullable).

Repo plugins/echo/src/db/repo/daily_reports.rs: DailyReport получил поле suggestions: serde_json::Value (default []). upsert принимает доп. параметр suggestions: &serde_json::Value (сериализуется в JSON-строку для TEXT-колонки). row_to_report читает индекс 6 и парсит робастно (NULL/невалидный JSON в []). API отдаёт уже распарсенный массив.

Генерация plugins/echo/src/daily_report/mod.rs: структуры SuggestedTask (title, description default, priority default 2) и ProjectSuggestions (project_path, project_name, tasks). После основного markdown-отчёта вызывается host.collect_project_activity(since_unix); если есть проекты, отдельный state.runner.one_shot с русским промптом SUGGEST_META_PROMPT, требующим строго JSON-массив. parse_suggestions_response снимает fenced-обёртки, берёт срез от первого [ до последнего ], парсит в Vec ProjectSuggestions; при любой ошибке пустой вектор (основной отчёт важнее, warn в лог). project_path в ответе совпадает с path проекта (ключ для POST /api/todos). Пустой день (NO_ACTIVITY_RU) в suggestions = [].

Тесты счётчика миграций обновлены 2 в 3: db/mod.rs, tests/db_init.rs, tests/phase6_repos_and_parsers.rs. Добавлен round-trip тест suggestions в daily_reports.rs.