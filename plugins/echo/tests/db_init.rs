//! Integration test для жизненного цикла Echo init() → DB → миграции.
//!
//! Проверяет:
//! - init() с custom db_path создаёт файл (включая parent-директории).
//! - Все таблицы из V001 присутствуют после первого init.
//! - Повторный init на том же пути не падает (миграции идемпотентны).

use forge_echo::EchoConfigStub;

#[tokio::test]
async fn init_creates_db_file_and_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("nested/echo.db");

    let cfg = EchoConfigStub {
        db_path: Some(db_path.clone()),
        ..EchoConfigStub::default()
    };
    let state = forge_echo::init(cfg).await.expect("init should succeed");
    assert!(db_path.exists(), "db file must be created at {db_path:?}");

    // Проверяем что все 6 prod-таблиц + schema_migrations присутствуют.
    let tables: Vec<String> = state
        .db
        .conn()
        .call(|c| {
            let mut stmt = c
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
            let it = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let collected: Result<Vec<_>, _> = it.collect();
            Ok(collected?)
        })
        .await
        .unwrap();

    for required in [
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
            "missing {required}, got {tables:?}"
        );
    }
}

#[tokio::test]
async fn init_is_idempotent_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("echo.db");

    let cfg1 = EchoConfigStub {
        db_path: Some(db_path.clone()),
        ..EchoConfigStub::default()
    };
    let _s1 = forge_echo::init(cfg1).await.unwrap();
    drop(_s1); // закроем connection (важно для WAL truncate)

    let cfg2 = EchoConfigStub {
        db_path: Some(db_path.clone()),
        ..EchoConfigStub::default()
    };
    let s2 = forge_echo::init(cfg2)
        .await
        .expect("second init must not fail");

    let n: i64 = s2
        .db
        .conn()
        .call(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    // Один entry на каждый embedded V*.sql (V001_init + V002_daily_reports),
    // не растёт при повторном init.
    assert_eq!(
        n, 2,
        "each embedded migration applied exactly once across two inits"
    );
}
