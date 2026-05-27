# Фича «Сводка дня» (Daily Summary)

End-to-end функция forge-echo: автоматическая и ручная генерация мотивационного markdown-отчёта за один локальный день («что сделано», «где я молодец», «на завтра»), его хранение и просмотр в UI.

## Backend (plugins/echo)

### Хранилище
- Миграция V002_daily_reports.sql создаёт таблицу daily_reports: id (UUID PK), day (TEXT YYYY-MM-DD, UNIQUE), content (markdown), source ('auto'|'manual'), created_at/updated_at (unix epoch). Один отчёт на день.
- Repo plugins/echo/src/db/repo/daily_reports.rs: upsert (ON CONFLICT(day), стабильный id/created_at), get_by_day, get, list(limit, day DESC).

### Генерация
- plugins/echo/src/daily_report/mod.rs::generate_report(state, host, day, source). Собирает за день chat-messages, tmux-pane снапшоты и git-активность (HostApi::collect_git_activity, since=начало дня). Пустые источники → content=NO_ACTIVITY_RU ('Сегодня активности не было') без вызова Claude. Иначе — русский prompt с 3 разделами через runner.one_shot, затем upsert по day. id стабилен между перегенерациями.

### Scheduler
- plugins/echo/src/daily_report/scheduler.rs — фоновый tokio-loop, около 23:00 local генерит отчёт за текущий день с source='auto'. Запускается из spawn_workers.

### REST API (plugins/echo/src/routes/daily_reports.rs, под /api/echo)
- GET /api/echo/daily-reports → {items:[...]} (list, day DESC).
- GET /api/echo/daily-reports/:day → отчёт за день; 400 на битый формат, 404 если нет.
- POST /api/echo/daily-reports/generate body {day?} → generate_report(source='manual'), таймаут ~90с; 200 c DTO, 400 на невалидный day. Без day → сегодня local.

## Plugin boundary
Все хост-данные (git, сессии) идут через trait echo_host_api::HostApi (метод collect_git_activity), реализация — EchoHostAdapter в tmux-web. Echo не зависит от AppState напрямую.

## Frontend (tmux-web, vanilla JS modules)
- core/markdown.js::renderMarkdownInto — безопасный рендер ограниченного markdown в DOM.
- daily-summary/daily-summary.js::showDailySummary — панель #daily-summary: грузит список (GET), рендерит markdown выбранного дня, навигация по датам, кнопка перегенерации (POST generate).
- settings/daily-summary-tab.js::renderDailySummaryTab — вкладка настроек с кнопками «Сгенерировать сейчас» и «Открыть страницу»; settings modal поддерживает initialTab='daily-summary'.

## Тесты
- Repo: upsert без дублей и со стабильным id, get_by_day/get_by_id, list ordering+limit.
- Генерация: пустой день → ровно NO_ACTIVITY_RU и запись в daily_reports без вызова runner; день с сообщениями → вызов mock-CLI и upsert; перегенерация сохраняет id (одна запись).
- Routes (mock-CLI + in-memory Db): generate 200/400, get_by_day 404/400, list+get roundtrip, default day=today local.
- Миграция: db::tests::migration_creates_daily_reports_table проверяет наличие таблицы и всех колонок (PRAGMA table_info) + migrate идемпотентен.

## Сборка
cargo build/test для forge-echo, devforge и --workspace — без ошибок и без warning'ов; cargo test --workspace зелёный.