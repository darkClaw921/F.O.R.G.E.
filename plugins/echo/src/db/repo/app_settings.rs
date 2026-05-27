//! KV-репозиторий над таблицей `app_settings`.
//!
//! Простое key/value-хранилище для рантайм-редактируемых настроек приложения
//! (например, пользовательские оверрайды промптов «Сводки дня»). В отличие от
//! доменных repo с богатыми struct'ами, здесь только строковые `value`:
//! интерпретация (plain text / JSON) — на стороне вызывающего кода.
//!
//! API:
//! - [`get`] — прочитать значение по ключу (`None`, если ключа нет);
//! - [`set`] — записать/обновить значение (upsert через `ON CONFLICT(key)`);
//! - [`delete`] — удалить ключ (сброс настройки к дефолту вызывающего кода).
//!
//! `updated_at` — unix-время (секунды, `chrono::Utc::now().timestamp()`),
//! как в остальных repo проекта.

use crate::db::Db;

/// Читает значение настройки по ключу. Возвращает `None`, если ключа нет.
pub async fn get(db: &Db, key: &str) -> anyhow::Result<Option<String>> {
    let key = key.to_string();
    let value = db
        .conn()
        .call(move |c| {
            let v = c
                .query_row(
                    "SELECT value FROM app_settings WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            Ok(v)
        })
        .await?;
    Ok(value)
}

/// Записывает значение настройки по ключу (upsert).
///
/// Если ключа нет — INSERT; если есть — обновляет `value` и `updated_at`
/// через `ON CONFLICT(key) DO UPDATE`.
pub async fn set(db: &Db, key: &str, value: &str) -> anyhow::Result<()> {
    let key = key.to_string();
    let value = value.to_string();
    let now = chrono::Utc::now().timestamp();
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO app_settings(key, value, updated_at) VALUES(?1, ?2, ?3) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                rusqlite::params![key, value, now],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Удаляет настройку по ключу (для сброса к дефолту). Отсутствие ключа — не ошибка.
pub async fn delete(db: &Db, key: &str) -> anyhow::Result<()> {
    let key = key.to_string();
    db.conn()
        .call(move |c| {
            c.execute(
                "DELETE FROM app_settings WHERE key = ?1",
                rusqlite::params![key],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
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
    async fn set_get_round_trip() {
        let db = fresh().await;
        assert!(get(&db, "report_prompt").await.unwrap().is_none());
        set(&db, "report_prompt", "hello").await.unwrap();
        assert_eq!(
            get(&db, "report_prompt").await.unwrap(),
            Some("hello".to_string())
        );
    }

    #[tokio::test]
    async fn set_on_conflict_updates_value() {
        let db = fresh().await;
        set(&db, "k", "v1").await.unwrap();
        set(&db, "k", "v2").await.unwrap();
        assert_eq!(get(&db, "k").await.unwrap(), Some("v2".to_string()));
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let db = fresh().await;
        set(&db, "k", "v").await.unwrap();
        delete(&db, "k").await.unwrap();
        assert!(get(&db, "k").await.unwrap().is_none());
        // delete несуществующего ключа — не ошибка.
        delete(&db, "missing").await.unwrap();
    }
}
