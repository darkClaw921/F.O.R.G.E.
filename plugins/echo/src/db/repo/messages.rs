//! CRUD для таблицы `messages` (записи чата).
//!
//! - `insert` — генерирует UUIDv4, ставит `created_at = now`. Возвращает
//!   собранный `Message` (UI сразу видит окончательный id).
//! - `list_by_session` — `ORDER BY created_at ASC` (диалог идёт сверху вниз);
//!   опциональный `before_ts` режет историю для пагинации старых сообщений.
//!   Использует составной индекс `idx_messages_session_created`.
//! - `delete_by_session` — массовая чистка (например при `/clear` чата).
//!
//! `content` — рендеренный текст (то, что показывает UI), `content_json` —
//! сырое содержимое события Claude CLI (tool_use, tool_result и т.п.) для
//! последующего реплея.

use serde::Serialize;

use crate::db::Db;

/// Запись сообщения.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub content_json: Option<String>,
    pub parent_id: Option<String>,
    pub created_at: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    db: &Db,
    session_id: &str,
    role: &str,
    content: &str,
    content_json: Option<&str>,
    parent_id: Option<&str>,
    tokens_in: i64,
    tokens_out: i64,
    cache_creation: i64,
    cache_read: i64,
) -> anyhow::Result<Message> {
    let m = Message {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        content: content.to_string(),
        content_json: content_json.map(|s| s.to_string()),
        parent_id: parent_id.map(|s| s.to_string()),
        created_at: chrono::Utc::now().timestamp(),
        tokens_in,
        tokens_out,
        cache_creation,
        cache_read,
    };
    let row = m.clone();
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO messages(\
                   id, session_id, role, content, content_json, parent_id, created_at,\
                   tokens_in, tokens_out, cache_creation, cache_read\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    row.id,
                    row.session_id,
                    row.role,
                    row.content,
                    row.content_json,
                    row.parent_id,
                    row.created_at,
                    row.tokens_in,
                    row.tokens_out,
                    row.cache_creation,
                    row.cache_read,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("messages::insert: {e}"))?;
    Ok(m)
}

/// Список сообщений сессии. `before_ts` (Some) — режет верхнюю границу
/// `created_at < before_ts` для пагинации старых. `limit` >0.
pub async fn list_by_session(
    db: &Db,
    session_id: &str,
    limit: i64,
    before_ts: Option<i64>,
) -> anyhow::Result<Vec<Message>> {
    let session_id = session_id.to_string();
    db.conn()
        .call(move |c| {
            let rows: Vec<Message> = if let Some(before) = before_ts {
                let mut stmt = c.prepare(
                    "SELECT id, session_id, role, content, content_json, parent_id, created_at,\
                            tokens_in, tokens_out, cache_creation, cache_read \
                     FROM messages WHERE session_id = ?1 AND created_at < ?2 \
                     ORDER BY created_at ASC LIMIT ?3",
                )?;
                let it =
                    stmt.query_map(rusqlite::params![session_id, before, limit], row_to_message)?;
                let collected: Result<Vec<_>, _> = it.collect();
                collected?
            } else {
                let mut stmt = c.prepare(
                    "SELECT id, session_id, role, content, content_json, parent_id, created_at,\
                            tokens_in, tokens_out, cache_creation, cache_read \
                     FROM messages WHERE session_id = ?1 \
                     ORDER BY created_at ASC LIMIT ?2",
                )?;
                let it = stmt.query_map(rusqlite::params![session_id, limit], row_to_message)?;
                let collected: Result<Vec<_>, _> = it.collect();
                collected?
            };
            Ok(rows)
        })
        .await
        .map_err(|e| anyhow::anyhow!("messages::list_by_session: {e}"))
}

pub async fn delete_by_session(db: &Db, session_id: &str) -> anyhow::Result<()> {
    let session_id = session_id.to_string();
    db.conn()
        .call(move |c| {
            c.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                rusqlite::params![session_id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("messages::delete_by_session: {e}"))?;
    Ok(())
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        content_json: row.get(4)?,
        parent_id: row.get(5)?,
        created_at: row.get(6)?,
        tokens_in: row.get(7)?,
        tokens_out: row.get(8)?,
        cache_creation: row.get(9)?,
        cache_read: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::chats;

    async fn fresh_with_session() -> (Db, String) {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let s = chats::create(&db, "t", None, "m").await.unwrap();
        (db, s.id)
    }

    #[tokio::test]
    async fn insert_and_list_orders_ascending() {
        let (db, sid) = fresh_with_session().await;
        for i in 0..5 {
            insert(
                &db,
                &sid,
                "user",
                &format!("hello {i}"),
                None,
                None,
                10,
                0,
                0,
                0,
            )
            .await
            .unwrap();
        }
        let rows = list_by_session(&db, &sid, 100, None).await.unwrap();
        assert_eq!(rows.len(), 5);
        // ASC order — first inserted first
        for (i, m) in rows.iter().enumerate() {
            assert_eq!(m.content, format!("hello {i}"));
        }
    }

    #[tokio::test]
    async fn cascade_delete_via_session() {
        let (db, sid) = fresh_with_session().await;
        insert(&db, &sid, "user", "x", None, None, 0, 0, 0, 0)
            .await
            .unwrap();
        chats::delete(&db, &sid).await.unwrap();
        let rows = list_by_session(&db, &sid, 10, None).await.unwrap();
        assert!(rows.is_empty(), "cascade FK should wipe messages");
    }

    #[tokio::test]
    async fn delete_by_session_clears_messages() {
        let (db, sid) = fresh_with_session().await;
        for _ in 0..3 {
            insert(&db, &sid, "user", "x", None, None, 0, 0, 0, 0)
                .await
                .unwrap();
        }
        delete_by_session(&db, &sid).await.unwrap();
        assert!(list_by_session(&db, &sid, 10, None).await.unwrap().is_empty());
        // session itself still exists
        assert!(chats::get(&db, &sid).await.unwrap().is_some());
    }
}
