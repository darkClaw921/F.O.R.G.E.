# plugins/echo/migrations/V001_init.sql

Initial migration для Echo plugin SQLite-БД. Создаёт 6 таблиц: chat_sessions (чаты), messages (сообщения с FK cascade на сессию + idx по session_id+created_at), autonomous_tasks (планировщик + idx по enabled+next_run_at), task_runs (лог запусков с FK cascade), memories (UNIQUE по scope+project_id+day, scope CHECK ('global_day','project','project_day')), token_stats (минутные bucket'ы для sparkline). project_id — soft-FK без REFERENCES (хост валидирует через HostApi). Timestamps — unix epoch INTEGER, кроме memories.day (TEXT YYYY-MM-DD UTC).
