# plugins/echo/src/db/repo/daily_reports.rs

Репозиторий таблицы daily_reports (plugins/echo/src/db/repo/daily_reports.rs) — хранилище «Сводок дня» Echo.

DailyReport struct (Serialize/Deserialize): id, day (YYYY-MM-DD, уникальный ключ), content (markdown), source ("auto"/"manual"), created_at, updated_at, и suggestions: serde_json::Value (serde default = empty_suggestions).

Поле suggestions (добавлено миграцией V003): JSON-массив предложений задач по проектам (формат ProjectSuggestions: project_path, project_name, tasks из объектов title/description/priority). В SQLite хранится в TEXT-колонке suggestions как JSON-строка. empty_suggestions() -> пустой JSON-массив (дефолт). parse_suggestions(Option<String>) парсит TEXT-колонку в serde_json::Value; при NULL/ошибке парса -> пустой массив.

upsert(db, day, content, source, suggestions: &serde_json::Value) -> DailyReport: insert-or-update по day. Если запись есть — UPDATE content+source+suggestions+updated_at, id сохраняется; иначе INSERT с новым id. suggestions сериализуется в JSON-строку (при ошибке -> "[]"). Возвращает итоговую запись с уже распарсенным suggestions.

Чтение: get(db, id), get_by_day(db, day), list(db, limit) — все SELECT'ят колонку suggestions и пропускают строку через row_to_report, который вызывает parse_suggestions для колонки suggestions.

Тесты: suggestions_round_trip проверяет, что suggestions сохраняется и читается через upsert/get_by_day/get, а пустой массив корректно перезаписывает непустой.

Связи: пишется из daily_report::generate_report; читается REST-роутом /api/echo/daily-reports, который отдаёт suggestions фронту (renderSuggestions).
