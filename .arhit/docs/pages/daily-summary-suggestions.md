# Сводка дня: предлагаемые задачи по проектам (forge-meoa)

Инкремент к фиче «Сводка дня» (forge-b4q). Внизу страницы сводки появляется блок «Предлагаемые задачи»: LLM-сгенерированные задачи, сгруппированные по проектам, в том же формате карточек что и в TODO-канбане, с возможностью выбрать какие добавить в TODO проекта. Существующее поведение не переписывается — только расширяется (таблица daily_reports, generate_report, REST /api/echo/daily-reports, фронт #daily-summary).

## Поток данных

collect_project_activity (HostApi) -> второй one_shot (JSON) в generate_suggestions -> колонка suggestions (daily_reports) -> REST /api/echo/daily-reports -> renderSuggestions (фронт).

1. HostApi::collect_project_activity(since_unix) (plugin boundary echo-host-api; реализация EchoHostAdapter в tmux-web/src/echo_host.rs) обходит tmux-сессии, дедуплицирует git-корни рабочих директорий и для каждого формирует ProjectActivity { path: git-корень, name: basename, git_log: коммиты дня }. Проект включается даже без коммитов (активен в сессии). В отличие от collect_git_activity (единый markdown для раздела «Что сделано»), здесь структурированный список.

2. daily_report::generate_report после основного markdown-отчёта вызывает collect_project_activity и передаёт результат в generate_suggestions — ОТДЕЛЬНЫЙ (второй) state.runner.one_shot с русским SUGGEST_META_PROMPT, требующим строго JSON-массив ProjectSuggestions { project_path, project_name, tasks[{title, description, priority}] }. project_path в ответе ДОЛЖЕН совпадать с ProjectActivity.path (ключ для POST /api/todos). git_log усекается до 2000 символов на проект. Любая ошибка (сбор/one_shot/парс) деградирует до пустого массива — основной отчёт важнее. parse_suggestions_response робастно снимает fenced-обёртки и берёт срез массива. Пустой день (NO_ACTIVITY_RU) -> suggestions = [].

3. Хранилище: миграция V003_daily_report_suggestions.sql добавляет TEXT-колонку suggestions (nullable) в daily_reports. daily_reports repo: поле suggestions: serde_json::Value (default []); upsert(db, day, content, source, suggestions) сериализует в JSON-строку; row_to_report парсит обратно (NULL/невалидный JSON -> []). get/get_by_day/list возвращают уже распарсенный массив.

4. REST /api/echo/daily-reports отдаёт suggestions как часть DailyReport.

5. Фронт renderSuggestions(suggestions) (daily-summary.js): для каждой группы — подзаголовок project_name, карточки задач (.kanban-card как в TODO: title, desc усечён до 140, P-pill приоритета) и кнопка «Добавить выбранные в TODO». Клик по карточке тоглит выбор (.selected); кнопка для каждой выбранной вызывает createTodoForPath(project_path, title, description) -> POST /api/todos (сервер делает resolve_root(path)). Успешные карточки помечаются .added + «✓ добавлено». Весь пользовательский контент рендерится через textContent (XSS-safe). Стили — daily-summary.css (.daily-summary-suggestions-*).

## Ключевые элементы

- ProjectActivity, HostApi::collect_project_activity (plugins/echo-host-api/src/lib.rs); реализация tmux-web/src/echo_host.rs.
- daily_report::generate_report, generate_suggestions, parse_suggestions_response, SuggestedTask, ProjectSuggestions (plugins/echo/src/daily_report/mod.rs).
- daily_reports repo + миграция V003 (plugins/echo/src/db/repo/daily_reports.rs).
- renderSuggestions (tmux-web/static/js/daily-summary/daily-summary.js), createTodoForPath (tmux-web/static/js/tasks/crud.js).

## Фазы

Phase 1 — backend: DTO + collect_project_activity, миграция V003, поле suggestions, генерация. Phase 2 — REST отдаёт suggestions. Phase 3 — фронт renderSuggestions + createTodoForPath + стили. Phase 4 — документация.