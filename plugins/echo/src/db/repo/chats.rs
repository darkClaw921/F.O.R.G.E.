//! CRUD для таблицы `chat_sessions`.
//!
//! Семантика:
//!
//! - `create` — генерирует UUID v4, проставляет `created_at = updated_at`.
//! - `list` — сортирует по `updated_at DESC` (последний активный — первым),
//!   фильтр по `project_id` опциональный.
//! - `delete` — каскадно сносит `messages` благодаря FK ON DELETE CASCADE.
//! - `touch_updated` — двигает `updated_at` (используется при инсёрте
//!   нового message в чат).
//!
//! `project_id` — soft-FK: запись с несуществующим в хост-`projects.id`
//! значением допустима, хост должен валидировать ввод сам.

use serde::Serialize;

use crate::db::Db;

/// Запись чат-сессии.
#[derive(Debug, Clone, Serialize)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Создаёт новый чат и возвращает заполненную запись.
pub async fn create(
    db: &Db,
    title: &str,
    project_id: Option<&str>,
    model: &str,
) -> anyhow::Result<ChatSession> {
    let id = uuid::Uuid::new_v4().to_string();
    create_with_id(db, &id, title, project_id, model).await
}

/// Создаёт чат с заранее заданным `id`. Нужно для Phase 4 scheduler'а:
/// служебные conversation для автономных задач именуются детерминированно
/// (`__autonomous__/<task_id>`), чтобы не плодить отдельную таблицу
/// маппинга task→conversation.
///
/// Возвращает ошибку (UNIQUE constraint), если запись с таким `id` уже
/// существует. Caller должен предварительно вызвать [`get`] для
/// «get-or-create» семантики.
pub async fn create_with_id(
    db: &Db,
    id: &str,
    title: &str,
    project_id: Option<&str>,
    model: &str,
) -> anyhow::Result<ChatSession> {
    let now = chrono::Utc::now().timestamp();
    let s = ChatSession {
        id: id.to_string(),
        title: title.to_string(),
        project_id: project_id.map(|s| s.to_string()),
        model: model.to_string(),
        created_at: now,
        updated_at: now,
    };
    let row = s.clone();
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO chat_sessions(id, title, project_id, model, created_at, updated_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    row.id,
                    row.title,
                    row.project_id,
                    row.model,
                    row.created_at,
                    row.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("chats::create_with_id: {e}"))?;
    Ok(s)
}

/// Список чатов. Если `project_id` задан — фильтр; иначе все.
/// `limit > 0`.
pub async fn list(
    db: &Db,
    project_id: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<ChatSession>> {
    let project = project_id.map(|s| s.to_string());
    db.conn()
        .call(move |c| {
            let rows: Vec<ChatSession> = if let Some(pid) = project {
                let mut stmt = c.prepare(
                    "SELECT id, title, project_id, model, created_at, updated_at \
                     FROM chat_sessions WHERE project_id = ?1 \
                     ORDER BY updated_at DESC LIMIT ?2",
                )?;
                let it = stmt.query_map(rusqlite::params![pid, limit], row_to_session)?;
                let collected: Result<Vec<_>, _> = it.collect();
                collected?
            } else {
                let mut stmt = c.prepare(
                    "SELECT id, title, project_id, model, created_at, updated_at \
                     FROM chat_sessions ORDER BY updated_at DESC LIMIT ?1",
                )?;
                let it = stmt.query_map(rusqlite::params![limit], row_to_session)?;
                let collected: Result<Vec<_>, _> = it.collect();
                collected?
            };
            Ok(rows)
        })
        .await
        .map_err(|e| anyhow::anyhow!("chats::list: {e}"))
}

/// Достаёт чат по id; `None` если нет.
pub async fn get(db: &Db, id: &str) -> anyhow::Result<Option<ChatSession>> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id, title, project_id, model, created_at, updated_at \
                     FROM chat_sessions WHERE id = ?1",
                    rusqlite::params![id],
                    row_to_session,
                )
                .ok();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("chats::get: {e}"))
}

/// Удаляет чат и все его messages (cascade FK).
pub async fn delete(db: &Db, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    db.conn()
        .call(move |c| {
            c.execute("DELETE FROM chat_sessions WHERE id = ?1", rusqlite::params![id])?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("chats::delete: {e}"))?;
    Ok(())
}

/// Сдвигает `updated_at` на текущее время.
pub async fn touch_updated(db: &Db, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    let now = chrono::Utc::now().timestamp();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
                rusqlite::params![now, id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("chats::touch_updated: {e}"))?;
    Ok(())
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatSession> {
    Ok(ChatSession {
        id: row.get(0)?,
        title: row.get(1)?,
        project_id: row.get(2)?,
        model: row.get(3)?,
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
    async fn create_get_list_delete() {
        let db = fresh().await;
        let s = create(&db, "First chat", None, "sonnet-4").await.unwrap();
        let got = get(&db, &s.id).await.unwrap().expect("must exist");
        assert_eq!(got.title, "First chat");
        assert!(got.project_id.is_none());

        let _ = create(&db, "Second", Some("proj-a"), "sonnet-4").await.unwrap();
        let all = list(&db, None, 10).await.unwrap();
        assert_eq!(all.len(), 2);
        let only_proj = list(&db, Some("proj-a"), 10).await.unwrap();
        assert_eq!(only_proj.len(), 1);

        delete(&db, &s.id).await.unwrap();
        assert!(get(&db, &s.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn touch_updates_timestamp() {
        let db = fresh().await;
        let s = create(&db, "T", None, "m").await.unwrap();
        // ensure at least one second elapses so updated_at strictly changes
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        touch_updated(&db, &s.id).await.unwrap();
        let got = get(&db, &s.id).await.unwrap().unwrap();
        assert!(got.updated_at > s.updated_at, "updated_at must advance");
    }

    #[tokio::test]
    async fn soft_fk_allows_unknown_project_id() {
        let db = fresh().await;
        // No projects table at all — should still insert OK.
        let s = create(&db, "X", Some("does-not-exist"), "m").await.unwrap();
        assert_eq!(s.project_id.as_deref(), Some("does-not-exist"));
    }
}
