# upsert

daily_reports::upsert(db, day, content, source) в plugins/echo/src/db/repo/daily_reports.rs. Upsert по UNIQUE(day): select-by-day + INSERT (новый UUIDv4) либо UPDATE content/source/updated_at с сохранением id и created_at. Возвращает DailyReport. Repo также: get_by_day(day)->Option, list(limit)->Vec (ORDER BY day DESC), get(id)->Option. Таблица создаётся миграцией V002_daily_reports.sql (id TEXT PK, day TEXT UNIQUE YYYY-MM-DD local, content, source 'auto'|'manual', created_at, updated_at). Зарегистрирован в db/repo/mod.rs.
