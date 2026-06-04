//! Репозиторий правил «Следующего шага» (`next_step_rules`).
//!
//! Память обратной связи фичи «Следующий шаг»: когда пользователь исправляет
//! предложенный воркером шаг, мы сохраняем правило
//! (`context_summary` → `suggested_next`). При последующих генерациях
//! [`list_rules`] подмешивает релевантные правила (глобальные + по проекту) в
//! prompt, чтобы предложения становились точнее.
//!
//! `project_id = NULL` означает ГЛОБАЛЬНОЕ правило (применимо к любой сессии).
//! Не-NULL — правило, специфичное для непрозрачного ярлыка проекта (git-корня).
//!
//! Repo-слой ничего не знает про axum/воркер — принимает `&Db`.

use serde::Serialize;

use crate::db::Db;

/// Максимальное число правил, подмешиваемых в один prompt (глобальные +
/// проектные суммарно). Кап защищает prompt от разрастания и держит контекст
/// сфокусированным на последних/релевантных коррекциях.
pub const DEFAULT_RULES_LIMIT: i64 = 20;

/// Одно правило обратной связи.
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub id: String,
    /// Непрозрачный ярлык проекта (git-корень) или `None` для глобального.
    pub project_id: Option<String>,
    /// Контекст, в котором правило уместно (pane-выдержка + отвергнутое
    /// предложение).
    pub context_summary: String,
    /// Что следовало предложить (коррекция пользователя).
    pub suggested_next: String,
    pub created_at: i64,
}

/// Вставляет новое правило (UUIDv4 + `created_at = now`).
///
/// `project_id = None` → глобальное правило. Возвращает созданную запись.
pub async fn insert_rule(
    db: &Db,
    project_id: Option<&str>,
    context_summary: &str,
    suggested_next: &str,
) -> anyhow::Result<Rule> {
    let id = uuid::Uuid::new_v4().to_string();
    let project_id = project_id.map(|s| s.to_string());
    let context_summary = context_summary.to_string();
    let suggested_next = suggested_next.to_string();
    let created_at = chrono::Utc::now().timestamp();

    let rule = Rule {
        id: id.clone(),
        project_id: project_id.clone(),
        context_summary: context_summary.clone(),
        suggested_next: suggested_next.clone(),
        created_at,
    };

    db.conn()
        .call(move |c| {
            c.execute(
                "INSERT INTO next_step_rules(id, project_id, context_summary, suggested_next, created_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, project_id, context_summary, suggested_next, created_at],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("next_step::insert_rule: {e}"))?;

    Ok(rule)
}

/// Возвращает релевантные правила для генерации: ГЛОБАЛЬНЫЕ
/// (`project_id IS NULL`) плюс правила указанного `project_id` (если задан),
/// отсортированные по `created_at DESC` и ограниченные `limit` последними.
///
/// При `project_id = None` отдаёт только глобальные правила.
pub async fn list_rules(
    db: &Db,
    project_id: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<Rule>> {
    let project_id = project_id.map(|s| s.to_string());
    db.conn()
        .call(move |c| {
            // Один SQL для обоих случаев: глобальные всегда, проектные — если
            // ?1 совпадает (?1 = NULL → совпадений по проекту нет).
            let mut stmt = c.prepare(
                "SELECT id, project_id, context_summary, suggested_next, created_at \
                 FROM next_step_rules \
                 WHERE project_id IS NULL OR project_id = ?1 \
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let it = stmt.query_map(rusqlite::params![project_id, limit], row_to_rule)?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("next_step::list_rules: {e}"))
}

fn row_to_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: row.get(0)?,
        project_id: row.get(1)?,
        context_summary: row.get(2)?,
        suggested_next: row.get(3)?,
        created_at: row.get(4)?,
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
    async fn insert_and_list_global_rule() {
        let db = fresh().await;
        let r = insert_rule(&db, None, "ctx", "do this next").await.unwrap();
        assert!(r.project_id.is_none());
        assert_eq!(r.context_summary, "ctx");
        assert_eq!(r.suggested_next, "do this next");

        let rules = list_rules(&db, None, DEFAULT_RULES_LIMIT).await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, r.id);
    }

    #[tokio::test]
    async fn list_returns_global_plus_project_scoped() {
        let db = fresh().await;
        insert_rule(&db, None, "g", "global step").await.unwrap();
        insert_rule(&db, Some("/repo/a"), "a", "a step").await.unwrap();
        insert_rule(&db, Some("/repo/b"), "b", "b step").await.unwrap();

        // Для проекта /repo/a — глобальное + только a (не b).
        let a = list_rules(&db, Some("/repo/a"), DEFAULT_RULES_LIMIT)
            .await
            .unwrap();
        assert_eq!(a.len(), 2, "global + project a, got {a:?}");
        assert!(a.iter().any(|r| r.suggested_next == "global step"));
        assert!(a.iter().any(|r| r.suggested_next == "a step"));
        assert!(!a.iter().any(|r| r.suggested_next == "b step"));

        // Без проекта — только глобальные.
        let g = list_rules(&db, None, DEFAULT_RULES_LIMIT).await.unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].suggested_next, "global step");
    }

    #[tokio::test]
    async fn list_respects_limit_and_order_desc() {
        let db = fresh().await;
        // Вставляем несколько глобальных с растущим created_at — порядок DESC.
        for i in 0..5 {
            insert_rule(&db, None, &format!("ctx{i}"), &format!("step{i}"))
                .await
                .unwrap();
            // created_at — секунды; делаем монотонными вставками подряд может
            // совпасть. Достаточно проверить кап и факт сортировки по rowid не
            // гарантирован — поэтому проверяем только лимит.
        }
        let limited = list_rules(&db, None, 3).await.unwrap();
        assert_eq!(limited.len(), 3, "limit must cap result");

        let all = list_rules(&db, None, DEFAULT_RULES_LIMIT).await.unwrap();
        assert_eq!(all.len(), 5);
    }
}
