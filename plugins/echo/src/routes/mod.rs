//! HTTP-routes плагина Echo.
//!
//! Все routes регистрируются под префиксом `/api/echo/*`. `build_router`
//! собирает их в один `Router` со state'ом `Arc<EchoState>` и мерджит с
//! хост-приложением через [`crate::register_routes`].
//!
//! Sub-модули:
//! - `memories` — CRUD over [`crate::db::repo::memories`].
//! - `conversations` — CRUD chat-сессий и листинг сообщений.
//! - `stats` — token_stats sparkline + POST cancel-run (Phase 3).
//! - `autonomous` — CRUD + run-now + runs-history для autonomous_tasks
//!   (Phase 4).

pub mod autonomous;
pub mod conversations;
pub mod memories;
pub mod memory;
pub mod stats;

use std::sync::Arc;

use axum::{routing::get, Router};

use crate::state::EchoState;

/// Health-check эндпоинт плагина.
///
/// Возвращает `text/plain` "ok" со статусом 200. Используется в smoke-тесте
/// (verify P1.10 / P2.11) и потенциально для liveness-probe.
async fn healthz() -> &'static str {
    "ok"
}

/// Собирает Router плагина и мерджит его с переданным `app`.
///
/// Routes регистрируются с префиксом `/api/echo/*`, что гарантирует
/// отсутствие коллизий с хост-routes. `EchoState` передаётся через
/// `with_state` — handlers Echo не видят `AppState` хоста напрямую.
pub fn build_router(state: Arc<EchoState>) -> Router {
    Router::new()
        .route("/api/echo/healthz", get(healthz))
        .merge(memories::router())
        .merge(memory::router())
        .merge(conversations::router())
        .merge(stats::router())
        .merge(autonomous::router())
        .route("/ws/echo", get(crate::ws::echo_ws))
        .with_state(state)
}
