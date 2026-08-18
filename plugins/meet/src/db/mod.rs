//! SQLite-хранилище плагина встреч.
//!
//! Тонкая обёртка над [`tokio_rusqlite::Connection`] плюс embedded-миграции из
//! `plugins/meet/migrations/V*.sql` (через `rust-embed`). Устроено дословно
//! как у Echo — и это осознанно: два разных механизма миграций в одном бинаре
//! означали бы два места, где можно забыть транзакцию.
//!
//! При открытии гарантируется:
//!
//! - `journal_mode = WAL` — читатели (батч бейджей, страница расшифровки) не
//!   блокируются писателем (поток сегментов идущей встречи);
//! - `foreign_keys = ON` — без него `ON DELETE CASCADE` в схеме был бы
//!   украшением, и удаление встречи оставляло бы сироты в пяти таблицах;
//! - миграции применяются идемпотентно, каждая в СВОЕЙ транзакции, и
//!   отмечаются в служебной `schema_migrations` по имени файла.
//!
//! # Герметичность
//!
//! `db_path` в тестах ВСЕГДА в `TempDir` (см. [`Db::open_memory`] и
//! `MeetConfig` с путями во временном каталоге). Иначе `cargo test` открывал
//! бы живую базу записей пользователя.

pub mod repo;

use std::path::Path;

use rust_embed::RustEmbed;
use tokio_rusqlite::Connection;

/// Embedded источник миграций (`plugins/meet/migrations/V*.sql`).
///
/// Путь указан относительно `CARGO_MANIFEST_DIR` крейта `forge-meet`, поэтому
/// файлы попадают в бинарь на этапе сборки и не зависят от `cwd` процесса.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/migrations/"]
struct Migrations;

/// Async-обёртка над SQLite-соединением плагина.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Открывает базу по указанному пути, создавая родительский каталог.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    anyhow::anyhow!("не создаётся каталог {}: {e}", parent.display())
                })?;
            }
        }
        let conn = Connection::open(path)
            .await
            .map_err(|e| anyhow::anyhow!("не открывается sqlite {}: {e}", path.display()))?;
        Ok(Self { conn })
    }

    /// Открывает базу в памяти — для тестов.
    pub async fn open_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()
            .await
            .map_err(|e| anyhow::anyhow!("не открывается sqlite в памяти: {e}"))?;
        Ok(Self { conn })
    }

    /// Применяет непрогнанные миграции в лексикографическом порядке имени.
    ///
    /// **Идемпотентна**: уже применённые пропускаются по `schema_migrations`.
    /// Повторный вызов на существующей базе — штатная ситуация (каждый старт
    /// devforge), а не восстановление после сбоя.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        // PRAGMA нельзя выполнять внутри транзакции, поэтому она первой.
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
            .map_err(|e| anyhow::anyhow!("не выставились прагмы: {e}"))?;

        let mut files: Vec<(String, Vec<u8>)> = Migrations::iter()
            .filter_map(|name| {
                let file = Migrations::get(name.as_ref())?;
                Some((name.to_string(), file.data.into_owned()))
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));

        if files.is_empty() {
            // Не паника и не ошибка: пустой каталог миграций означает, что
            // сборка потеряла ассеты, и об этом надо узнать из лога, а не из
            // падения devforge.
            tracing::warn!(target: "forge_meet", "forge-meet: миграций не найдено");
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
            .map_err(|e| anyhow::anyhow!("не читается schema_migrations: {e}"))?;

        for (name, data) in files {
            if applied.iter().any(|a| a == &name) {
                continue;
            }
            let sql = String::from_utf8(data)
                .map_err(|e| anyhow::anyhow!("миграция {name} не UTF-8: {e}"))?;
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
                .map_err(|e| anyhow::anyhow!("миграция {name_for_log} не применилась: {e}"))?;
            tracing::info!(target: "forge_meet", migration = %name_for_log, "forge-meet: миграция применена");
        }

        Ok(())
    }

    /// Нижележащее соединение для репозиториев.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn migrated() -> Db {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let db = migrated().await;
        // Второй и третий прогон — ровно то, что делает каждый старт devforge.
        db.migrate().await.unwrap();
        db.migrate().await.unwrap();

        let applied: i64 = db
            .conn()
            .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(applied, 1, "миграция обязана отметиться ровно один раз");
    }

    #[tokio::test]
    async fn every_table_of_the_plan_exists() {
        let db = migrated().await;
        let tables: Vec<String> = db
            .conn()
            .call(|c| {
                let mut stmt =
                    c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
                let out = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .unwrap();
        for expected in [
            "meetings",
            "meeting_sources",
            "meeting_participants",
            "meeting_speaker_spans",
            "meeting_segments",
            "meeting_events",
        ] {
            assert!(tables.iter().any(|t| t == expected), "нет таблицы {expected}: {tables:?}");
        }
    }

    /// Индекс, ради которого «одна активная запись на комнату» — свойство
    /// базы, а не мьютекса.
    #[tokio::test]
    async fn the_partial_unique_index_exists_and_is_partial() {
        let db = migrated().await;
        let sql: String = db
            .conn()
            .call(|c| {
                Ok(c.query_row(
                    "SELECT sql FROM sqlite_master WHERE type='index' AND name='ux_meetings_active_url'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert!(sql.contains("UNIQUE"), "{sql}");
        assert!(sql.contains("WHERE"), "индекс обязан быть частичным: {sql}");
    }

    /// `WITHOUT ROWID` у `meeting_sources` — не украшение: именно он делает
    /// батч ленты одним сканом по префиксу первичного ключа.
    #[tokio::test]
    async fn the_sources_table_is_the_index_itself() {
        let db = migrated().await;
        let sql: String = db
            .conn()
            .call(|c| {
                Ok(c.query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='meeting_sources'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert!(sql.contains("WITHOUT ROWID"), "{sql}");
    }

    /// Каскад обязан быть настоящим: без `foreign_keys = ON` он молча не
    /// работает, и удаление встречи оставляло бы сироты в пяти таблицах.
    #[tokio::test]
    async fn cascade_delete_actually_cascades() {
        let db = migrated().await;
        db.conn()
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO meetings(id,url_key,canonical_url,platform,room_id,status,\
                        summary_status,mode,created_at,updated_at,meeting_dir) \
                     VALUES('m1','k','https://x/j/1','telemost','1','recording','none','normal',1,1,'/tmp/m1');\
                     INSERT INTO meeting_sources(plugin,chat_id,message_id,meeting_id,created_at) \
                     VALUES('telegram','7',42,'m1',1);\
                     INSERT INTO meeting_events(meeting_id,at,from_status,to_status) \
                     VALUES('m1',1,NULL,'queued');",
                )?;
                c.execute("DELETE FROM meetings WHERE id='m1'", [])?;
                Ok(())
            })
            .await
            .unwrap();

        let (sources, events): (i64, i64) = db
            .conn()
            .call(|c| {
                Ok((
                    c.query_row("SELECT COUNT(*) FROM meeting_sources", [], |r| r.get(0))?,
                    c.query_row("SELECT COUNT(*) FROM meeting_events", [], |r| r.get(0))?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(sources, 0, "источник пережил удаление встречи");
        assert_eq!(events, 0, "журнал пережил удаление встречи");
    }

    /// Основа дедупа `attached: true`: вторая активная запись на тот же
    /// `url_key` физически не создаётся.
    #[tokio::test]
    async fn a_second_active_recording_of_the_same_room_is_impossible() {
        let db = migrated().await;
        let insert = |id: &'static str, status: &'static str| {
            let db_conn = db.conn().clone();
            async move {
                db_conn
                    .call(move |c| {
                        c.execute(
                            "INSERT INTO meetings(id,url_key,canonical_url,platform,room_id,status,\
                                summary_status,mode,created_at,updated_at,meeting_dir) \
                             VALUES(?1,'telemost:host/1','https://x/j/1','telemost','1',?2,'none','normal',1,1,'/tmp')",
                            rusqlite::params![id, status],
                        )?;
                        Ok(())
                    })
                    .await
            }
        };

        insert("a", "recording").await.unwrap();
        let second = insert("b", "queued").await;
        assert!(second.is_err(), "вторая активная запись на ту же комнату создалась");

        // А после завершения первой — новая запись законна: индекс частичный.
        db.conn()
            .call(|c| {
                c.execute("UPDATE meetings SET status='ready' WHERE id='a'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        insert("c", "queued").await.expect("завершённая встреча не должна блокировать новую");
    }
}
