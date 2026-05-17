//! Aggregation для `token_stats` — минутные bucket'ы потребления токенов.
//!
//! Используется UI sparkline (last N minutes) и autonomous-cap (Phase 6).
//!
//! - `add_tokens` — атомарный upsert (SQLite UPSERT через `ON CONFLICT`)
//!   в bucket `ts_unix / 60`. Конкурентные вызовы корректно сложат
//!   значения благодаря `excluded.tokens_in + tokens_in`.
//! - `range` — возвращает СУЩЕСТВУЮЩИЕ bucket'ы (sparse). UI должен сам
//!   заполнять пустые минуты нулями — это сознательное упрощение, чтобы
//!   не растить таблицу нулевыми записями.
//! - `sum_last_minutes` — суммирует `[now-n*60, now]`.
//! - `sum_for_day` — суммирует за UTC-день (для дневного cap'а).

use serde::Serialize;

use crate::db::Db;

#[derive(Debug, Clone, Serialize)]
pub struct TokenStatBucket {
    pub bucket_minute: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenStatSum {
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

/// Атомарно прибавляет токены в bucket = `ts_unix / 60`. Если bucket'а
/// ещё нет — INSERT с этими значениями.
pub async fn add_tokens(
    db: &Db,
    ts_unix: i64,
    tokens_in: i64,
    tokens_out: i64,
    cache_creation: i64,
    cache_read: i64,
) -> anyhow::Result<()> {
    let bucket = ts_unix / 60;
    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO token_stats(bucket_minute, tokens_in, tokens_out, cache_creation, cache_read) \
                 VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(bucket_minute) DO UPDATE SET \
                   tokens_in      = tokens_in      + excluded.tokens_in,\
                   tokens_out     = tokens_out     + excluded.tokens_out,\
                   cache_creation = cache_creation + excluded.cache_creation,\
                   cache_read     = cache_read     + excluded.cache_read",
                rusqlite::params![bucket, tokens_in, tokens_out, cache_creation, cache_read],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("stats::add_tokens: {e}"))?;
    Ok(())
}

/// Сырые bucket'ы в диапазоне `[from_bucket, to_bucket]` (включительно),
/// отсортированные по времени. Пустые минуты НЕ возвращаются — UI
/// дополняет нулями (см. doc-комментарий модуля).
pub async fn range(
    db: &Db,
    from_bucket: i64,
    to_bucket: i64,
) -> anyhow::Result<Vec<TokenStatBucket>> {
    db.conn()
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT bucket_minute, tokens_in, tokens_out, cache_creation, cache_read \
                 FROM token_stats WHERE bucket_minute BETWEEN ?1 AND ?2 \
                 ORDER BY bucket_minute ASC",
            )?;
            let it = stmt.query_map(rusqlite::params![from_bucket, to_bucket], |row| {
                Ok(TokenStatBucket {
                    bucket_minute: row.get(0)?,
                    tokens_in: row.get(1)?,
                    tokens_out: row.get(2)?,
                    cache_creation: row.get(3)?,
                    cache_read: row.get(4)?,
                })
            })?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("stats::range: {e}"))
}

/// Суммирует за последние `n` минут от текущего момента.
pub async fn sum_last_minutes(db: &Db, n: i64) -> anyhow::Result<TokenStatSum> {
    let now = chrono::Utc::now().timestamp();
    let from_bucket = (now / 60) - n.max(0);
    let to_bucket = now / 60;
    sum_range(db, from_bucket, to_bucket).await
}

/// Суммирует за UTC-день, заданный границами `[day_start_unix, day_end_unix)`.
pub async fn sum_for_day(
    db: &Db,
    day_start_unix: i64,
    day_end_unix: i64,
) -> anyhow::Result<TokenStatSum> {
    let from_bucket = day_start_unix / 60;
    // day_end_unix - 1 чтобы не захватить начало следующих суток.
    let to_bucket = (day_end_unix - 1) / 60;
    sum_range(db, from_bucket, to_bucket).await
}

async fn sum_range(db: &Db, from_bucket: i64, to_bucket: i64) -> anyhow::Result<TokenStatSum> {
    db.conn()
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT \
                       COALESCE(SUM(tokens_in), 0),\
                       COALESCE(SUM(tokens_out), 0),\
                       COALESCE(SUM(cache_creation), 0),\
                       COALESCE(SUM(cache_read), 0) \
                     FROM token_stats WHERE bucket_minute BETWEEN ?1 AND ?2",
                    rusqlite::params![from_bucket, to_bucket],
                    |row| {
                        Ok(TokenStatSum {
                            tokens_in: row.get(0)?,
                            tokens_out: row.get(1)?,
                            cache_creation: row.get(2)?,
                            cache_read: row.get(3)?,
                        })
                    },
                )
                .unwrap_or_default();
            Ok(res)
        })
        .await
        .map_err(|e| anyhow::anyhow!("stats::sum_range: {e}"))
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
    async fn add_tokens_accumulates_in_same_bucket() {
        let db = fresh().await;
        let ts = 1_700_000_000_i64; // фиксированный bucket = 28_333_333
        for _ in 0..10 {
            add_tokens(&db, ts, 5, 1, 0, 2).await.unwrap();
        }
        let bucket = ts / 60;
        let rows = range(&db, bucket, bucket).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens_in, 50);
        assert_eq!(rows[0].tokens_out, 10);
        assert_eq!(rows[0].cache_read, 20);
    }

    #[tokio::test]
    async fn range_returns_only_existing_buckets() {
        let db = fresh().await;
        let base = 1_700_000_000_i64;
        add_tokens(&db, base, 1, 0, 0, 0).await.unwrap();
        add_tokens(&db, base + 120, 2, 0, 0, 0).await.unwrap(); // +2 мин
        let from = base / 60;
        let to = (base + 120) / 60;
        let rows = range(&db, from, to).await.unwrap();
        // Middle bucket пустой — он не возвращается.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bucket_minute, from);
        assert_eq!(rows[1].bucket_minute, to);
    }

    #[tokio::test]
    async fn sum_for_day_aggregates() {
        let db = fresh().await;
        // 2026-05-17 00:00 UTC = ts 1779580800
        let day_start = 1_779_580_800_i64;
        let day_end = day_start + 86_400;
        add_tokens(&db, day_start + 60, 100, 50, 0, 0).await.unwrap();
        add_tokens(&db, day_start + 3600, 200, 25, 0, 0).await.unwrap();
        // Шумовой bucket за пределами окна.
        add_tokens(&db, day_end + 60, 999, 0, 0, 0).await.unwrap();
        let sum = sum_for_day(&db, day_start, day_end).await.unwrap();
        assert_eq!(sum.tokens_in, 300);
        assert_eq!(sum.tokens_out, 75);
    }
}
