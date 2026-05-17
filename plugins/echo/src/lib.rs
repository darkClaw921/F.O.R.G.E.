//! # forge-echo
//!
//! Плагин Echo для F.O.R.G.E. — встроенный чат-ассистент на базе Claude CLI
//! (`claude -p --output-format stream-json`). Архитектурно оформлен как
//! отдельный крейт в Cargo workspace, чтобы:
//!
//! - Изолировать код от основного `tmux-web` бинаря.
//! - Создать прецедент plugin-системы для F.O.R.G.E.
//! - Дать предсказуемую границу через [`echo_host_api::HostApi`] (вместо
//!   передачи `AppState` напрямую).
//!
//! ## Lifecycle (применяется в `tmux-web/src/main.rs`)
//!
//! ```ignore
//! let echo_state = forge_echo::init(EchoConfigStub::default()).await?;
//! let host: Arc<dyn HostApi> = Arc::new(EchoHostAdapter { state: app_state.clone() });
//! let app = forge_echo::register_routes(app, echo_state.clone(), host.clone());
//! // Phase 4:
//! // forge_echo::spawn_workers(&echo_state, host.clone());
//! ```
//!
//! `register_routes` ОБЯЗАТЕЛЬНО вызывать ДО `.layer(auth_middleware)`,
//! чтобы Bearer-auth покрывал `/api/echo/*` и `/ws/echo` в remote-mode
//! автоматически (мы наследуем существующую axum-идиому хоста).

pub mod actions;
pub mod claude;
pub mod config;
pub mod db;
pub mod memory;
pub mod routes;
pub mod scheduler;
pub mod state;
pub mod ws;

use std::sync::Arc;

use axum::Router;

pub use config::EchoConfig;
pub use echo_host_api::HostApi;
pub use state::{EchoConfigStub, EchoState, ServerEvent};

/// Инициализирует Echo state из [`EchoConfigStub`] (legacy для тестов и
/// упрощённой инициализации).
///
/// Внутри конструирует полный [`EchoConfig`] из дефолтов + переопределений
/// `EchoConfigStub` (db_path / cli_path / max_parallel_runs), затем
/// делегирует в [`init_with_config`].
pub async fn init(cfg: EchoConfigStub) -> anyhow::Result<Arc<EchoState>> {
    let mut full = EchoConfig::default();
    if let Some(p) = cfg.db_path {
        full.db_path = p;
    }
    if let Some(p) = cfg.cli_path {
        full.cli_path = p;
    }
    if let Some(n) = cfg.max_parallel_runs {
        full.max_parallel_runs = n;
    }
    full.validate_and_fix();
    init_with_config(full).await
}

/// Инициализирует Echo state из полной [`EchoConfig`].
///
/// `async` потому что `Db::open` + `migrate` асинхронные. ClaudeRunner
/// синхронный (`new` не падает на отсутствующем CLI — только warn-log).
///
/// # Errors
///
/// Возвращает ошибку при невозможности создать parent-директорию,
/// открыть SQLite-файл или применить миграции.
pub async fn init_with_config(cfg: EchoConfig) -> anyhow::Result<Arc<EchoState>> {
    tracing::info!(target: "forge_echo", path = %cfg.db_path.display(), "forge-echo: opening SQLite");
    let db = db::Db::open(&cfg.db_path).await?;
    db.migrate().await?;
    tracing::info!(target: "forge_echo", path = %cfg.db_path.display(), "forge-echo: DB initialized");

    let runner = Arc::new(claude::ClaudeRunner::new(
        cfg.cli_path.clone(),
        cfg.max_parallel_runs,
    ));
    tracing::info!(
        target: "forge_echo",
        max_parallel = cfg.max_parallel_runs,
        default_model = %cfg.default_model,
        capture_lines = cfg.capture_lines,
        autonomous_max_tokens_per_day = cfg.autonomous_max_tokens_per_day,
        "forge-echo: ClaudeRunner ready"
    );

    Ok(Arc::new(EchoState::new_with_config(
        Arc::new(db),
        runner,
        cfg,
    )))
}

/// Регистрирует routes плагина в переданном `Router` и привязывает
/// [`HostApi`] adapter к state.
///
/// Возвращает обновлённый `Router`, который вызывающий код мерджит с
/// остальным приложением. Phase 1 регистрирует только `/api/echo/healthz`.
///
/// # Panics
///
/// Если `host` уже был установлен (повторный вызов `register_routes`).
/// Это не должно происходить в нормальном lifecycle.
pub fn register_routes(
    app: Router,
    state: Arc<EchoState>,
    host: Arc<dyn HostApi>,
) -> Router {
    if state.host.set(host).is_err() {
        tracing::warn!("forge-echo: host adapter already set, ignoring");
    }
    let echo_router = routes::build_router(state);
    tracing::info!(
        "forge-echo: registered routes (/api/echo/healthz, /api/echo/memories*, /api/echo/conversations*, /api/echo/stats, /api/echo/run/:id/cancel, /api/echo/autonomous-tasks*, /ws/echo)"
    );
    app.merge(echo_router)
}

/// Phase 6 hardening — graceful shutdown плагина Echo.
///
/// Шаги:
///   1. `state.shutdown.cancel()` — будит scheduler/memory loop'ы.
///   2. `shutdown_workers` — abort'ит сохранённые JoinHandle'ы.
///   3. `runner.shutdown()` — abort'ит активные Claude run'ы; `kill_on_drop`
///      убивает дочерние процессы CLI.
///
/// Безопасно вызывать многократно (CancellationToken идемпотентен; drain
/// JoinHandle'ов — тоже).
///
/// SQLite-connection держится в `Arc<Db>`. Явный `close` не нужен:
/// `tokio_rusqlite::Connection` корректно завершает worker-поток на Drop
/// последнего владельца Arc. При активном WAL-журнале это безопасно —
/// данные на диске консистентны.
pub async fn shutdown(state: &Arc<EchoState>) {
    tracing::info!(target: "forge_echo", "forge-echo: shutdown initiated");
    state.shutdown.cancel();
    state.shutdown_workers().await;
    state.runner.shutdown().await;
    tracing::info!(target: "forge_echo", "forge-echo: shutdown complete");
}

/// Спавнит фоновые worker'ы плагина.
///
/// Phase 4 запускает autonomous scheduler ([`scheduler::spawn`]) — фоновый
/// loop, который каждые 5 секунд опрашивает `autonomous_tasks` и
/// исполняет due-задачи через [`scheduler::runner::run_task`]. JoinHandle
/// не возвращается наружу — handle сохраняется в `state.workers` для
/// graceful shutdown (Phase 6 hardening).
///
/// Phase 5+ здесь же появятся memory-rollover loop и другие background
/// задачи плагина.
pub fn spawn_workers(state: &Arc<EchoState>, host: Arc<dyn HostApi>) {
    let scheduler_handle = scheduler::spawn(state.clone(), host.clone());
    state.register_worker(scheduler_handle);
    let memory_handle = memory::scheduler::spawn(state.clone(), host);
    state.register_worker(memory_handle);
    tracing::info!(target: "forge_echo", "forge-echo: spawn_workers (scheduler + memory rollover started)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_cancels_token_and_clears_workers() {
        let db = db::Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        let runner = Arc::new(claude::ClaudeRunner::new(
            std::path::PathBuf::from("/nope"),
            1,
        ));
        let state = Arc::new(EchoState::new(Arc::new(db), runner));

        // Симулируем background worker.
        let token_for_worker = state.shutdown.clone();
        let handle = tokio::spawn(async move {
            token_for_worker.cancelled().await;
        });
        state.register_worker(handle);
        // Дать spawn-task'е добавить handle в vec.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!state.shutdown.is_cancelled());
        shutdown(&state).await;
        assert!(state.shutdown.is_cancelled());
        // workers очищены.
        assert!(state.workers.lock().await.is_empty());

        // Повторный вызов безопасен.
        shutdown(&state).await;
    }

    #[tokio::test]
    async fn init_with_config_uses_provided_values() {
        let db_path = std::env::temp_dir().join(format!(
            "echo_init_test_{}.db",
            uuid::Uuid::new_v4()
        ));
        let cfg = EchoConfig {
            db_path: db_path.clone(),
            max_parallel_runs: 7,
            default_model: "test-model".into(),
            ..EchoConfig::default()
        };

        let state = init_with_config(cfg.clone()).await.unwrap();
        assert_eq!(state.config.max_parallel_runs, 7);
        assert_eq!(state.config.default_model, "test-model");
        assert_eq!(state.config.db_path, db_path);

        let _ = std::fs::remove_file(&db_path);
    }
}
