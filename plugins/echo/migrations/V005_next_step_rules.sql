-- V005_next_step_rules.sql — память правил «Следующего шага».
--
-- Фича «Следующий шаг» учится на обратной связи пользователя: когда он
-- исправляет предложенный воркером шаг (feedback), мы сохраняем правило
-- (context_summary → suggested_next), которое подмешивается в последующие
-- генерации, чтобы предложения становились точнее.
--
-- Поля:
--   * id              — UUIDv4 (PRIMARY KEY).
--   * project_id      — непрозрачный ярлык проекта (git-корень) или NULL.
--                       NULL означает ГЛОБАЛЬНОЕ правило (применяется ко всем
--                       сессиям независимо от проекта).
--   * context_summary — краткое описание контекста, в котором правило уместно
--                       (обычно: pane-выдержка + отвергнутое предложение).
--   * suggested_next  — что НА САМОМ ДЕЛЕ следовало предложить (коррекция).
--   * created_at      — unix-время создания.
--
-- Notes:
--   * rust-embed подхватит файл по имени, трекинг в schema_migrations.
--   * Индекс по project_id ускоряет list_rules(project_id) (глобальные +
--     проектные).

CREATE TABLE IF NOT EXISTS next_step_rules (
  id              TEXT PRIMARY KEY,
  project_id      TEXT,
  context_summary TEXT NOT NULL,
  suggested_next  TEXT NOT NULL,
  created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_next_step_rules_project
  ON next_step_rules(project_id, created_at DESC);
