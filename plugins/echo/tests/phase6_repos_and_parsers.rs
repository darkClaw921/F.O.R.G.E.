//! Phase 6 — integration-тесты для repo/parser пограничных случаев.
//!
//! Большая часть unit-coverage уже встроена в `#[cfg(test)]` секции каждого
//! модуля (см. plugins/echo/src/**); этот файл фиксирует **сквозные
//! сценарии**, которые требуют комбинировать несколько repo-функций или
//! проверяют edge-cases, заявленные в acceptance-criteria задачи 661.8.
//!
//! Сценарии:
//! - `db_migrate_*` — повторная миграция идемпотентна.
//! - `repo_chats_*` — полный CRUD: create → get → list → delete.
//! - `repo_messages_*` — insert 100 сообщений → list по conv_id отсортирован.
//! - `repo_memories_*` — upsert по натуральному ключу не плодит дублей.
//! - `repo_autonomous_find_due_*` — edge-cases: next_run_at = now, NULL, > now.
//! - `repo_stats_sum_for_day_*` — sanity-check агрегации.
//! - `autonomous_sum_tokens_since_*` — поведение cap-агрегации (Phase 6.5).
//! - `parser_actions_*` — фикстуры: empty / valid prompt / valid system /
//!   unknown name / malformed JSON.
//! - `parser_events_*` — NDJSON с/без cache_* полей.

use forge_echo::actions::{parser, Action, SystemActionKind};
use forge_echo::claude::events::{parse_line, ClaudeEvent};
use forge_echo::db::repo::memories::MemoryScope;
use forge_echo::db::repo::{autonomous, chats, memories, messages, stats};
use forge_echo::db::Db;

async fn fresh_db() -> Db {
    let db = Db::open_memory().await.unwrap();
    db.migrate().await.unwrap();
    db
}

#[tokio::test]
async fn db_migrate_is_idempotent_in_integration_context() {
    let db = fresh_db().await;
    // Повторно — ничего не должно сломаться, schema_migrations не должен расти.
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let count: i64 = db
        .conn()
        .call(|c| {
            let n: i64 =
                c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    // Один entry на каждый embedded V*.sql (V001_init + V002_daily_reports +
    // V003_daily_report_suggestions + V004_app_settings + V005_next_step_rules).
    assert_eq!(count, 5);
}

#[tokio::test]
async fn repo_chats_full_crud_cycle() {
    let db = fresh_db().await;
    let c = chats::create(&db, "first", None, "sonnet").await.unwrap();
    assert!(!c.id.is_empty());
    let got = chats::get(&db, &c.id).await.unwrap().unwrap();
    assert_eq!(got.title, "first");

    let list = chats::list(&db, None, 20).await.unwrap();
    assert!(list.iter().any(|x| x.id == c.id));

    chats::delete(&db, &c.id).await.unwrap();
    assert!(chats::get(&db, &c.id).await.unwrap().is_none());
}

#[tokio::test]
async fn repo_messages_bulk_insert_and_list_ordered() {
    let db = fresh_db().await;
    let chat = chats::create(&db, "bulk", None, "m").await.unwrap();
    // 100 user-сообщений.
    for i in 0..100 {
        messages::insert(
            &db,
            &chat.id,
            "user",
            &format!("msg-{i}"),
            None,
            None,
            0,
            0,
            0,
            0,
        )
        .await
        .unwrap();
    }
    let list = messages::list_by_session(&db, &chat.id, 200, None)
        .await
        .unwrap();
    assert_eq!(list.len(), 100);
    // Сортировка по created_at ASC (см. repo::messages).
    for w in list.windows(2) {
        assert!(w[0].created_at <= w[1].created_at);
    }
}

#[tokio::test]
async fn repo_memories_upsert_does_not_duplicate_natural_key() {
    let db = fresh_db().await;
    // 5 upsert'ов с одинаковым (scope, project_id, day) → одна запись.
    let scope = MemoryScope::GlobalDay;
    for i in 0..5 {
        memories::upsert(
            &db,
            scope,
            None,
            Some("2026-05-17"),
            &format!("body v{i}"),
            "test",
        )
        .await
        .unwrap();
    }
    let listed = memories::list(&db, Some(scope), None, Some("2026-05-17"))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "upsert must not duplicate by natural key");
    assert_eq!(listed[0].content, "body v4", "последний upsert виден");
}

#[tokio::test]
async fn repo_autonomous_find_due_edge_cases() {
    let db = fresh_db().await;
    let now = chrono::Utc::now().timestamp();

    let t_eq = autonomous::create_task(&db, "eq", "p", 60, "m", None)
        .await
        .unwrap();
    autonomous::set_next_run(&db, &t_eq.id, now).await.unwrap();

    let t_past = autonomous::create_task(&db, "past", "p", 60, "m", None)
        .await
        .unwrap();
    autonomous::set_next_run(&db, &t_past.id, now - 100)
        .await
        .unwrap();

    let t_future = autonomous::create_task(&db, "future", "p", 60, "m", None)
        .await
        .unwrap();
    autonomous::set_next_run(&db, &t_future.id, now + 100)
        .await
        .unwrap();

    // task с NULL next_run_at: create_task сразу проставляет next_run_at,
    // поэтому явно обнулим через прямой UPDATE.
    let t_null = autonomous::create_task(&db, "null", "p", 60, "m", None)
        .await
        .unwrap();
    let id_for_null = t_null.id.clone();
    db.conn()
        .call(move |c| {
            c.execute(
                "UPDATE autonomous_tasks SET next_run_at = NULL WHERE id = ?1",
                rusqlite::params![id_for_null],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let due = autonomous::find_due(&db, now).await.unwrap();
    let ids: Vec<&str> = due.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&t_eq.id.as_str()), "next_run_at == now is due");
    assert!(ids.contains(&t_past.id.as_str()), "next_run_at < now is due");
    assert!(
        !ids.contains(&t_future.id.as_str()),
        "future task is not due"
    );
    assert!(
        !ids.contains(&t_null.id.as_str()),
        "NULL next_run_at is not due"
    );
}

#[tokio::test]
async fn repo_stats_sum_for_day_sanity() {
    let db = fresh_db().await;
    let day_start = 1_779_580_800_i64; // 2026-05-17 00:00 UTC
    stats::add_tokens(&db, day_start + 10, 50, 5, 0, 0).await.unwrap();
    stats::add_tokens(&db, day_start + 600, 60, 6, 0, 0).await.unwrap();
    // Шум — за пределами окна.
    stats::add_tokens(&db, day_start + 86_400 + 10, 100, 0, 0, 0)
        .await
        .unwrap();
    let s = stats::sum_for_day(&db, day_start, day_start + 86_400)
        .await
        .unwrap();
    assert_eq!(s.tokens_in, 110);
    assert_eq!(s.tokens_out, 11);
}

#[tokio::test]
async fn autonomous_sum_tokens_since_filters_by_started_at() {
    let db = fresh_db().await;
    let t = autonomous::create_task(&db, "x", "p", 60, "m", None)
        .await
        .unwrap();

    // run "очень давно".
    let r_old = autonomous::insert_run(&db, &t.id, 100).await.unwrap();
    autonomous::finish_run(&db, &r_old.id, "success", None, 1, 2, None)
        .await
        .unwrap();
    // run "сегодня".
    let now = chrono::Utc::now().timestamp();
    let r_today = autonomous::insert_run(&db, &t.id, now).await.unwrap();
    autonomous::finish_run(&db, &r_today.id, "success", None, 100, 50, None)
        .await
        .unwrap();

    let today_start = now - 3600;
    let used = autonomous::sum_tokens_since(&db, today_start)
        .await
        .unwrap();
    assert_eq!(used, 150);
}

#[test]
fn parser_actions_empty_returns_empty() {
    assert!(parser::extract("").is_empty());
}

#[test]
fn parser_actions_valid_prompt_block() {
    let txt = "intro\n```forge-actions\n[{\"id\":\"p1\",\"label\":\"Detail\",\"kind\":\"prompt\",\"text\":\"explain\"}]\n```\nend";
    let r = parser::extract(txt);
    assert_eq!(r.len(), 1);
    assert!(matches!(r[0], Action::Prompt { .. }));
}

#[test]
fn parser_actions_valid_system_block_known_name() {
    // `create_task` есть в SystemActionKind enum (см. actions/mod.rs).
    let txt = "x\n```forge-actions\n[{\"id\":\"s1\",\"label\":\"Make\",\"kind\":\"system\",\"name\":\"create_task\",\"params\":{\"title\":\"X\"}}]\n```\ny";
    let r = parser::extract(txt);
    assert_eq!(r.len(), 1);
    match &r[0] {
        Action::System { name, .. } => {
            assert_eq!(*name, SystemActionKind::CreateTask);
        }
        other => panic!("expected System action, got {other:?}"),
    }
}

#[test]
fn parser_actions_unknown_system_name_skipped() {
    let txt = "```forge-actions\n[{\"id\":\"x\",\"label\":\"X\",\"kind\":\"system\",\"name\":\"made.up.name\",\"params\":{}}]\n```";
    assert!(parser::extract(txt).is_empty());
}

#[test]
fn parser_actions_malformed_json_returns_empty() {
    let txt = "```forge-actions\n[not valid json}}\n```";
    assert!(parser::extract(txt).is_empty());
}

#[test]
fn parser_events_with_cache_fields() {
    let with_cache = r#"{"type":"result","usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":3,"cache_read_input_tokens":4}}"#;
    let ev = parse_line(with_cache).expect("must parse");
    match ev {
        ClaudeEvent::Result { usage, .. } => {
            assert_eq!(usage.input_tokens, 1);
            assert_eq!(usage.output_tokens, 2);
            assert_eq!(usage.cache_creation_input_tokens, 3);
            assert_eq!(usage.cache_read_input_tokens, 4);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parser_events_without_cache_fields_default_to_zero() {
    let without_cache = r#"{"type":"result","usage":{"input_tokens":10,"output_tokens":20}}"#;
    let ev = parse_line(without_cache).expect("must parse");
    match ev {
        ClaudeEvent::Result { usage, .. } => {
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.cache_creation_input_tokens, 0);
            assert_eq!(usage.cache_read_input_tokens, 0);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
