-- V004_app_settings.sql — kv-хранилище рантайм-редактируемых настроек приложения.
--
-- Простая key/value-таблица для оверрайдов, которые пользователь может менять
-- на лету (например, кастомные промпты «Сводки дня»). `value` хранится как
-- TEXT (произвольная строка/JSON по усмотрению вызывающего кода), `updated_at`
-- — unix-время последней записи.
--
-- Notes:
--   * key — PRIMARY KEY, repo использует INSERT ... ON CONFLICT(key) DO UPDATE.
--   * rust-embed подхватит файл по имени, трекинг в schema_migrations.

CREATE TABLE IF NOT EXISTS app_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
