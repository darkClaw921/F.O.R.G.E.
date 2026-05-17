//! CRUD для таблицы `memories` (роллап-заметок для prompt injection).
//!
//! Семантика scope:
//!
//! - `GlobalDay`  — глобальный summary дня (`project_id=NULL`, `day=YYYY-MM-DD`).
//! - `Project`    — постоянная справка по проекту (`project_id=Some`, `day=NULL`).
//! - `ProjectDay` — дневной summary конкретного проекта (`project_id=Some`,
//!   `day=YYYY-MM-DD`).
//!
//! Триплет `(scope, project_id, day)` — UNIQUE. `upsert` использует
//! `ON CONFLICT DO UPDATE` чтобы заменять `content` без пересоздания id
//! (важно для UI, который держит ссылку на memory.id).

use serde::{Deserialize, Serialize};

use crate::db::Db;

/// Skope memory. Сериализуется в snake_case под существующие SQL-литералы.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    GlobalDay,
    Project,
    ProjectDay,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryScope::GlobalDay => "global_day",
            MemoryScope::Project => "project",
            MemoryScope::ProjectDay => "project_day",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global_day" => Some(Self::GlobalDay),
            "project" => Some(Self::Project),
            "project_day" => Some(Self::ProjectDay),
            _ => None,
        }
    }
}

/// Запись memory.
#[derive(Debug, Clone, Serialize)]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub project_id: Option<String>,
    pub day: Option<String>,
    pub content: String,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Upsert по UNIQUE(scope, project_id, day):
/// - если записи нет — INSERT с новым UUIDv4;
/// - если есть — UPDATE content+source+updated_at, id сохраняется.
///
/// Возвращает итоговую запись (после операции).
pub async fn upsert(
    db: &Db,
    scope: MemoryScope,
    project_id: Option<&str>,
    day: Option<&str>,
    content: &str,
    source: &str,
) -> anyhow::Result<Memory> {
    let scope_str = scope.as_str().to_string();
    let project = project_id.map(|s| s.to_string());
    let day = day.map(|s| s.to_string());
    let content = content.to_string();
    let source = source.to_string();
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    db.conn()
        .call(move |c| {
            // UNIQUE constraints на NULL в SQLite не работают как «один NULL»,
            // поэтому делаем явный select-by-key + INSERT либо UPDATE.
            let existing: Option<(String, i64)> = c
                .query_row(
                    "SELECT id, created_at FROM memories \
                     WHERE scope = ?1 \
                       AND (project_id IS ?2 OR project_id = ?2) \
                       AND (day IS ?3 OR day = ?3)",
                    rusqlite::params![scope_str, project, day],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();

            if let Some((id, created_at)) = existing {
                c.execute(
                    "UPDATE memories SET content = ?1, source = ?2, updated_at = ?3 WHERE id = ?4",
                    rusqlite::params![content, source, now, id],
                )?;
                Ok(Memory {
                    id,
                    scope: scope_str,
                    project_id: project,
                    day,
                    content,
                    source,
                    created_at,
                    updated_at: now,
                })
            } else {
                c.execute(
                    "INSERT INTO memories(id, scope, project_id, day, content, source, created_at, updated_at) \
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    rusqlite::params![new_id, scope_str, project, day, content, source, now],
                )?;
                Ok(Memory {
                    id: new_id,
                    scope: scope_str,
                    project_id: project,
                    day,
                    content,
                    source,
                    created_at: now,
                    updated_at: now,
                })
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("memories::upsert: {e}"))
}

/// Список memories с опциональными фильтрами. Сортировка `updated_at DESC`.
pub async fn list(
    db: &Db,
    scope: Option<MemoryScope>,
    project_id: Option<&str>,
    day: Option<&str>,
) -> anyhow::Result<Vec<Memory>> {
    let scope_str = scope.map(|s| s.as_str().to_string());
    let project = project_id.map(|s| s.to_string());
    let day = day.map(|s| s.to_string());
    db.conn()
        .call(move |c| {
            // Динамический WHERE без расцветания комбинаций: каждый фильтр
            // даёт пару условий `?N IS NULL OR column = ?N`.
            let mut stmt = c.prepare(
                "SELECT id, scope, project_id, day, content, source, created_at, updated_at \
                 FROM memories \
                 WHERE (?1 IS NULL OR scope = ?1) \
                   AND (?2 IS NULL OR project_id = ?2) \
                   AND (?3 IS NULL OR day = ?3) \
                 ORDER BY updated_at DESC",
            )?;
            let it = stmt.query_map(rusqlite::params![scope_str, project, day], row_to_memory)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memories::list: {e}"))
}

pub async fn get(db: &Db, id: &str) -> anyhow::Result<Option<Memory>> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id, scope, project_id, day, content, source, created_at, updated_at \
                     FROM memories WHERE id = ?1",
                    rusqlite::params![id],
                    row_to_memory,
                )
                .ok();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memories::get: {e}"))
}

pub async fn delete(db: &Db, id: &str) -> anyhow::Result<usize> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let n = c.execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
            Ok(n)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memories::delete: {e}"))
}

/// Обновляет только `content` + двигает `updated_at`. Возвращает кол-во
/// затронутых строк (0 → 404 на уровне route).
pub async fn patch(db: &Db, id: &str, content: &str) -> anyhow::Result<usize> {
    let id = id.to_string();
    let content = content.to_string();
    let now = chrono::Utc::now().timestamp();
    db.conn()
        .call(move |c| {
            let n = c.execute(
                "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![content, now, id],
            )?;
            Ok(n)
        })
        .await
        .map_err(|e| anyhow::anyhow!("memories::patch: {e}"))
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get(0)?,
        scope: row.get(1)?,
        project_id: row.get(2)?,
        day: row.get(3)?,
        content: row.get(4)?,
        source: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> Db {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    #[tokio::test]
    async fn upsert_replaces_on_same_key() {
        let db = fresh().await;
        let a = upsert(
            &db,
            MemoryScope::GlobalDay,
            None,
            Some("2026-05-17"),
            "A",
            "auto",
        )
        .await
        .unwrap();
        let b = upsert(
            &db,
            MemoryScope::GlobalDay,
            None,
            Some("2026-05-17"),
            "B",
            "auto",
        )
        .await
        .unwrap();
        assert_eq!(a.id, b.id, "id must be stable on upsert");
        assert_eq!(b.content, "B");
        let all = list(&db, None, None, None).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn list_filters() {
        let db = fresh().await;
        upsert(&db, MemoryScope::GlobalDay, None, Some("d1"), "g1", "auto")
            .await
            .unwrap();
        upsert(&db, MemoryScope::Project, Some("p1"), None, "p1", "auto")
            .await
            .unwrap();
        upsert(
            &db,
            MemoryScope::ProjectDay,
            Some("p1"),
            Some("d1"),
            "pd1",
            "auto",
        )
        .await
        .unwrap();

        let only_p1 = list(&db, None, Some("p1"), None).await.unwrap();
        assert_eq!(only_p1.len(), 2);
        let only_day = list(&db, None, None, Some("d1")).await.unwrap();
        assert_eq!(only_day.len(), 2);
        let only_scope = list(&db, Some(MemoryScope::Project), None, None).await.unwrap();
        assert_eq!(only_scope.len(), 1);
        let exact = list(&db, Some(MemoryScope::ProjectDay), Some("p1"), Some("d1"))
            .await
            .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].content, "pd1");
    }

    #[tokio::test]
    async fn patch_and_delete() {
        let db = fresh().await;
        let m = upsert(&db, MemoryScope::Project, Some("p"), None, "x", "manual")
            .await
            .unwrap();
        assert_eq!(patch(&db, &m.id, "y").await.unwrap(), 1);
        assert_eq!(get(&db, &m.id).await.unwrap().unwrap().content, "y");
        assert_eq!(patch(&db, "nope", "z").await.unwrap(), 0);
        assert_eq!(delete(&db, &m.id).await.unwrap(), 1);
        assert!(get(&db, &m.id).await.unwrap().is_none());
        assert_eq!(delete(&db, "nope").await.unwrap(), 0);
    }
}
