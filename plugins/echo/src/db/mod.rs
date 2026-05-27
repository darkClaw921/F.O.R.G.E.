//! SQLite-хранилище плагина Echo.
//!
//! Тонкая обёртка над [`tokio_rusqlite::Connection`] + embedded-миграции из
//! `plugins/echo/migrations/V*.sql` (через `rust-embed`). При открытии БД
//! гарантируется:
//!
//! - `journal_mode = WAL` — параллельные read'ы не блокируются write'ом.
//! - `foreign_keys = ON` — `messages.session_id` и `task_runs.task_id`
//!   реально каскадно удаляются.
//! - Все миграции применяются идемпотентно и атомарно в одной транзакции;
//!   каждая записывается в служебную таблицу `schema_migrations` по имени файла.
//!
//! Использование:
//!
//! ```ignore
//! use forge_echo::db::Db;
//! let db = Db::open_memory().await?;
//! db.migrate().await?;
//! ```

pub mod repo;

use std::path::Path;

use rust_embed::RustEmbed;
use tokio_rusqlite::Connection;

/// Embedded источник миграций (`plugins/echo/migrations/V*.sql`).
///
/// Folder указан относительно `CARGO_MANIFEST_DIR` крейта `forge-echo`, поэтому
/// файлы попадают в бинарь на этапе сборки и не зависят от `cwd` процесса.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/migrations/"]
struct Migrations;

/// Async-обёртка над SQLite-соединением.
///
/// Клонировать через [`Db::handle`] не нужно — `Connection` сам внутри
/// `Arc`-овая. Если потребуется shared-владение между несколькими
/// акторами, используйте `Arc<Db>`.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Открывает БД по указанному пути.
    ///
    /// Создаёт родительскую директорию (рекурсивно) если её ещё нет —
    /// это важно для дефолтного пути `~/.config/forge/echo.db` при
    /// первом запуске.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    anyhow::anyhow!("failed to create parent dir {}: {e}", parent.display())
                })?;
            }
        }
        let conn = Connection::open(path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to open sqlite at {}: {e}", path.display()))?;
        Ok(Self { conn })
    }

    /// Открывает БД в памяти (для тестов).
    ///
    /// WAL для `:memory:` бесполезен, но `migrate` его всё равно попробует
    /// включить (SQLite просто оставит journal_mode=`memory` и не упадёт).
    pub async fn open_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()
            .await
            .map_err(|e| anyhow::anyhow!("failed to open in-memory sqlite: {e}"))?;
        Ok(Self { conn })
    }

    /// Применяет все pending-миграции в лексикографическом порядке имени
    /// файла (V001_init.sql, V002_..., ...).
    ///
    /// Идемпотентна: уже применённые миграции пропускаются по таблице
    /// `schema_migrations`. Включает WAL + foreign_keys прагмы перед
    /// применением — это безопасно вызывать многократно.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        // PRAGMA нельзя вызывать в транзакции, поэтому применяем сначала.
        self.conn
            .call(|c| {
                c.pragma_update(None, "journal_mode", "WAL")?;
                c.pragma_update(None, "foreign_keys", "ON")?;
                c.execute_batch(
                    "CREATE TABLE IF NOT EXISTS schema_migrations (\
                       name TEXT PRIMARY KEY,\
                       applied_at INTEGER NOT NULL\
                     )",
                )?;
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("pragma setup failed: {e}"))?;

        // Соберём миграции, отсортируем по имени.
        let mut files: Vec<(String, Vec<u8>)> = Migrations::iter()
            .filter_map(|name| {
                let file = Migrations::get(name.as_ref())?;
                Some((name.to_string(), file.data.into_owned()))
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));

        if files.is_empty() {
            tracing::warn!("forge-echo: no migrations found in embedded folder");
            return Ok(());
        }

        let applied: Vec<String> = self
            .conn
            .call(|c| {
                let mut stmt = c.prepare("SELECT name FROM schema_migrations")?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| anyhow::anyhow!("read schema_migrations failed: {e}"))?;

        for (name, data) in files {
            if applied.iter().any(|a| a == &name) {
                tracing::debug!(migration = %name, "forge-echo: skip already-applied");
                continue;
            }
            let sql = String::from_utf8(data)
                .map_err(|e| anyhow::anyhow!("migration {name} is not UTF-8: {e}"))?;
            let name_for_log = name.clone();
            self.conn
                .call(move |c| {
                    let tx = c.transaction()?;
                    tx.execute_batch(&sql)?;
                    tx.execute(
                        "INSERT INTO schema_migrations(name, applied_at) VALUES(?1, ?2)",
                        rusqlite::params![name, chrono::Utc::now().timestamp()],
                    )?;
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(|e| anyhow::anyhow!("migration {name_for_log} failed: {e}"))?;
            tracing::info!(migration = %name_for_log, "forge-echo: migration applied");
        }

        Ok(())
    }

    /// Доступ к нижележащему async-connection для repo-слоя.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrate_creates_all_tables() {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();

        let tables: Vec<String> = db
            .conn()
            .call(|c| {
                let mut stmt =
                    c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap();

        for required in [
            "app_settings",
            "autonomous_tasks",
            "chat_sessions",
            "daily_reports",
            "memories",
            "messages",
            "schema_migrations",
            "task_runs",
            "token_stats",
        ] {
            assert!(
                tables.iter().any(|t| t == required),
                "missing table {required}; got {tables:?}"
            );
        }
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        db.migrate().await.unwrap();
        let applied: i64 = db
            .conn()
            .call(|c| {
                let n: i64 =
                    c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
                Ok(n)
            })
            .await
            .unwrap();
        // Каждый V*.sql-файл регистрируется по имени один раз. Повторный
        // migrate() не должен добавлять дубли — поэтому count == число файлов
        // (V001 + V002 + V003 + V004), а не растёт при повторных прогонах.
        assert_eq!(applied, 4, "expected one entry per embedded migration file");
    }

    #[tokio::test]
    async fn migration_creates_daily_reports_table() {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        // V002 должна создать таблицу daily_reports с ожидаемыми колонками.
        let cols: Vec<String> = db
            .conn()
            .call(|c| {
                let mut stmt = c.prepare("PRAGMA table_info(daily_reports)")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(1))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        assert!(!cols.is_empty(), "daily_reports table must exist after migrate()");
        for expected in [
            "id",
            "day",
            "content",
            "source",
            "created_at",
            "updated_at",
            "suggestions",
        ] {
            assert!(
                cols.iter().any(|c| c == expected),
                "daily_reports must have column {expected}, got {cols:?}"
            );
        }
    }

    #[tokio::test]
    async fn foreign_keys_enabled() {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let fk_on: i64 = db
            .conn()
            .call(|c| {
                let n: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
                Ok(n)
            })
            .await
            .unwrap();
        assert_eq!(fk_on, 1);
    }
}
