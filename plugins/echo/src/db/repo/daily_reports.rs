//! CRUD для таблицы `daily_reports` («Сводка дня»).
//!
//! В отличие от `memories` (внутренний артефакт автоматизации), `daily_report`
//! — пользовательская сущность: один markdown-отчёт на локальный день
//! (`day` = `YYYY-MM-DD`), который открывается как отрендеренная страница.
//!
//! `day` — UNIQUE, поэтому [`upsert`] использует select-by-day + INSERT/UPDATE,
//! сохраняя стабильный `id` при перегенерации (важно для UI-ссылок).
//! `source` — `'auto'` (scheduler ~23:00 local) или `'manual'` (кнопка).

use serde::Serialize;

use crate::db::Db;

/// Запись отчёта за день.
#[derive(Debug, Clone, Serialize)]
pub struct DailyReport {
    pub id: String,
    /// Локальная дата в формате `YYYY-MM-DD` (UNIQUE).
    pub day: String,
    /// Markdown-содержимое отчёта.
    pub content: String,
    /// Источник генерации: `'auto'` | `'manual'`.
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Upsert по UNIQUE(day):
/// - если записи за `day` нет — INSERT с новым UUIDv4;
/// - если есть — UPDATE content+source+updated_at, `id` сохраняется.
///
/// Возвращает итоговую запись (после операции).
pub async fn upsert(
    db: &Db,
    day: &str,
    content: &str,
    source: &str,
) -> anyhow::Result<DailyReport> {
    let day = day.to_string();
    let content = content.to_string();
    let source = source.to_string();
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    db.conn()
        .call(move |c| {
            let existing: Option<(String, i64)> = c
                .query_row(
                    "SELECT id, created_at FROM daily_reports WHERE day = ?1",
                    rusqlite::params![day],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();

            if let Some((id, created_at)) = existing {
                c.execute(
                    "UPDATE daily_reports SET content = ?1, source = ?2, updated_at = ?3 WHERE id = ?4",
                    rusqlite::params![content, source, now, id],
                )?;
                Ok(DailyReport {
                    id,
                    day,
                    content,
                    source,
                    created_at,
                    updated_at: now,
                })
            } else {
                c.execute(
                    "INSERT INTO daily_reports(id, day, content, source, created_at, updated_at) \
                     VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
                    rusqlite::params![new_id, day, content, source, now],
                )?;
                Ok(DailyReport {
                    id: new_id,
                    day,
                    content,
                    source,
                    created_at: now,
                    updated_at: now,
                })
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("daily_reports::upsert: {e}"))
}

/// Возвращает отчёт за конкретный день (`YYYY-MM-DD`) или `None`.
pub async fn get_by_day(db: &Db, day: &str) -> anyhow::Result<Option<DailyReport>> {
    let day = day.to_string();
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id, day, content, source, created_at, updated_at \
                     FROM daily_reports WHERE day = ?1",
                    rusqlite::params![day],
                    row_to_report,
                )
                .ok();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("daily_reports::get_by_day: {e}"))
}

/// Возвращает последние `limit` отчётов, отсортированные по `day DESC`.
pub async fn list(db: &Db, limit: i64) -> anyhow::Result<Vec<DailyReport>> {
    db.conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, day, content, source, created_at, updated_at \
                 FROM daily_reports ORDER BY day DESC LIMIT ?1",
            )?;
            let it = stmt.query_map(rusqlite::params![limit], row_to_report)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("daily_reports::list: {e}"))
}

/// Возвращает отчёт по `id` или `None`.
pub async fn get(db: &Db, id: &str) -> anyhow::Result<Option<DailyReport>> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id, day, content, source, created_at, updated_at \
                     FROM daily_reports WHERE id = ?1",
                    rusqlite::params![id],
                    row_to_report,
                )
                .ok();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("daily_reports::get: {e}"))
}

fn row_to_report(row: &rusqlite::Row<'_>) -> rusqlite::Result<DailyReport> {
    Ok(DailyReport {
        id: row.get(0)?,
        day: row.get(1)?,
        content: row.get(2)?,
        source: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
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
    async fn upsert_replaces_on_same_day() {
        let db = fresh().await;
        let a = upsert(&db, "2026-05-27", "A", "auto").await.unwrap();
        let b = upsert(&db, "2026-05-27", "B", "manual").await.unwrap();
        assert_eq!(a.id, b.id, "id must be stable on upsert");
        assert_eq!(b.content, "B");
        assert_eq!(b.source, "manual");
        assert_eq!(b.created_at, a.created_at, "created_at preserved");
        let all = list(&db, 100).await.unwrap();
        assert_eq!(all.len(), 1, "upsert must not create a duplicate");
    }

    #[tokio::test]
    async fn get_by_day_and_get_by_id() {
        let db = fresh().await;
        let r = upsert(&db, "2026-05-20", "hello", "auto").await.unwrap();
        let by_day = get_by_day(&db, "2026-05-20").await.unwrap().unwrap();
        assert_eq!(by_day.id, r.id);
        let by_id = get(&db, &r.id).await.unwrap().unwrap();
        assert_eq!(by_id.day, "2026-05-20");
        assert!(get_by_day(&db, "1999-01-01").await.unwrap().is_none());
        assert!(get(&db, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_orders_by_day_desc() {
        let db = fresh().await;
        upsert(&db, "2026-05-18", "c", "auto").await.unwrap();
        upsert(&db, "2026-05-20", "a", "auto").await.unwrap();
        upsert(&db, "2026-05-19", "b", "auto").await.unwrap();
        let all = list(&db, 100).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].day, "2026-05-20");
        assert_eq!(all[1].day, "2026-05-19");
        assert_eq!(all[2].day, "2026-05-18");
        let limited = list(&db, 2).await.unwrap();
        assert_eq!(limited.len(), 2);
    }
}
