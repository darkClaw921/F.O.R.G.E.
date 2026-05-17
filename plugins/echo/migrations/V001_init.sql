-- V001_init.sql — initial schema for forge-echo plugin
--
-- Tables (in dependency order):
--   chat_sessions     — long-lived chat threads
--   messages          — individual chat messages (FK chat_sessions, cascade)
--   autonomous_tasks  — scheduled prompt jobs
--   task_runs         — execution log for autonomous_tasks (FK, cascade)
--   memories          — daily/project rollup notes injected into prompts
--   token_stats       — minute-level token usage buckets
--
-- Notes:
--   * project_id is intentionally a soft-FK (no REFERENCES) because the
--     authoritative projects store lives in the host (tmux-web). Existence
--     is validated in code via HostApi::list_projects.
--   * All timestamps are unix epoch seconds (INTEGER), except `day` which
--     is a textual UTC date (YYYY-MM-DD).
--   * `content_json` keeps the structured Claude tool-event payload alongside
--     the text rendering (`content`) for replay / UI.

CREATE TABLE chat_sessions (
  id         TEXT    PRIMARY KEY,
  title      TEXT    NOT NULL,
  project_id TEXT    NULL,
  model      TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE messages (
  id             TEXT    PRIMARY KEY,
  session_id     TEXT    NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
  role           TEXT    NOT NULL CHECK(role IN ('user','assistant','system','tool')),
  content        TEXT    NOT NULL,
  content_json   TEXT    NULL,
  parent_id      TEXT    NULL,
  created_at     INTEGER NOT NULL,
  tokens_in      INTEGER NOT NULL DEFAULT 0,
  tokens_out     INTEGER NOT NULL DEFAULT 0,
  cache_creation INTEGER NOT NULL DEFAULT 0,
  cache_read     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_messages_session_created ON messages(session_id, created_at);

CREATE TABLE autonomous_tasks (
  id               TEXT    PRIMARY KEY,
  name             TEXT    NOT NULL,
  prompt_template  TEXT    NOT NULL,
  interval_seconds INTEGER NOT NULL,
  model            TEXT    NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1,
  project_id       TEXT    NULL,
  last_run_at      INTEGER NULL,
  next_run_at      INTEGER NULL,
  created_at       INTEGER NOT NULL
);
CREATE INDEX idx_autonomous_enabled_next ON autonomous_tasks(enabled, next_run_at);

CREATE TABLE task_runs (
  id                TEXT    PRIMARY KEY,
  task_id           TEXT    NOT NULL REFERENCES autonomous_tasks(id) ON DELETE CASCADE,
  started_at        INTEGER NOT NULL,
  finished_at       INTEGER NULL,
  status            TEXT    NOT NULL CHECK(status IN ('running','success','error','cancelled')),
  result_message_id TEXT    NULL,
  tokens_in         INTEGER NOT NULL DEFAULT 0,
  tokens_out        INTEGER NOT NULL DEFAULT 0,
  error             TEXT    NULL
);

CREATE TABLE memories (
  id         TEXT    PRIMARY KEY,
  scope      TEXT    NOT NULL CHECK(scope IN ('global_day','project','project_day')),
  project_id TEXT    NULL,
  day        TEXT    NULL,
  content    TEXT    NOT NULL,
  source     TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(scope, project_id, day)
);

CREATE TABLE token_stats (
  bucket_minute  INTEGER PRIMARY KEY,
  tokens_in      INTEGER NOT NULL DEFAULT 0,
  tokens_out     INTEGER NOT NULL DEFAULT 0,
  cache_creation INTEGER NOT NULL DEFAULT 0,
  cache_read     INTEGER NOT NULL DEFAULT 0
);
