-- V002_daily_reports.sql — отдельная сущность «Сводка дня» для forge-echo.
--
-- Таблица daily_reports хранит сгенерированный markdown-отчёт за один
-- локальный день (YYYY-MM-DD). В отличие от `memories` (внутренний артефакт
-- автоматизации, scope=global_day), daily_report — это пользовательская
-- сущность: открывается как отрендеренная страница, генерируется по кнопке
-- или авто-scheduler'ом ~23:00 local.
--
-- Notes:
--   * `day` — UNIQUE для ON CONFLICT-upsert (один отчёт на день).
--   * `source` — 'auto' (scheduler) | 'manual' (кнопка).
--   * timestamps — unix epoch seconds (INTEGER).

CREATE TABLE daily_reports (
  id         TEXT    PRIMARY KEY,
  day        TEXT    NOT NULL UNIQUE,   -- YYYY-MM-DD (local)
  content    TEXT    NOT NULL,          -- markdown
  source     TEXT    NOT NULL,          -- 'auto' | 'manual'
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
