//! Plugin boundary для Echo плагина.
//!
//! Этот крейт определяет [`HostApi`] — trait, через который Echo получает
//! доступ к хост-системе (tmux, projects, auth). Конкретная реализация
//! живёт в `tmux-web/src/echo_host.rs` (`EchoHostAdapter`), которая
//! оборачивает `Arc<AppState>` без утечки его в плагин.
//!
//! Phase 1 — в плагине ещё нет реальной интеграции, методы возвращают
//! заглушки. Реальные impl появляются в Phase 3 (`forge-fa3.2`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Информация о tmux-сессии для Echo (упрощённый view).
///
/// Минимальный набор полей, нужный плагину: имя сессии (для capture-pane),
/// количество окон и панелей (для контекстного prompt).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    /// Имя tmux-сессии (уникально в рамках сервера).
    pub name: String,
    /// Количество окон в сессии.
    pub windows: u32,
    /// Количество панелей суммарно во всех окнах.
    pub panes: u32,
}

/// Информация о проекте F.O.R.G.E. для Echo.
///
/// Используется в `list_projects` для UI выбора проекта в чате и для
/// soft-FK на `projects.id` в SQLite-таблицах Echo (`chat_sessions`,
/// `memories`, `autonomous_tasks`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    /// Slug-id проекта (`[a-z0-9_-]+`).
    pub id: String,
    /// Человекочитаемое имя.
    pub name: String,
    /// Абсолютный путь к корню проекта.
    pub path: String,
}

/// Plugin boundary: всё, что Echo может попросить у хоста.
///
/// `Send + Sync` обязательно — adapter живёт в `Arc<dyn HostApi>` и
/// передаётся между tokio-тасками. `async_trait` нужен для
/// dyn-совместимости (нативный `async fn` в trait не object-safe).
///
/// # Контракт
///
/// - [`list_sessions`](HostApi::list_sessions) — текущие tmux-сессии хоста.
///   Может вернуть пустой вектор если tmux-сервер не запущен.
/// - [`capture_pane_full`](HostApi::capture_pane_full) — `tmux capture-pane -p`
///   на указанную сессию с N строками истории. Используется prompt builder'ом
///   для подмешивания контекста.
/// - [`list_projects`](HostApi::list_projects) — все зарегистрированные проекты
///   из `ProjectStore`.
/// - [`active_project_id`](HostApi::active_project_id) — id активного сейчас
///   проекта (для дефолтной фильтрации чатов и memory scope).
/// - [`auth_token`](HostApi::auth_token) — Bearer-token в remote-mode
///   (`None` в localhost-режиме). Echo использует его для WS-протокола,
///   когда плагин сам обращается к хост-API.
#[async_trait]
pub trait HostApi: Send + Sync {
    async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>>;

    async fn capture_pane_full(&self, session: &str, lines: i32) -> anyhow::Result<String>;

    async fn list_projects(&self) -> anyhow::Result<Vec<ProjectInfo>>;

    async fn active_project_id(&self) -> Option<String>;

    fn auth_token(&self) -> Option<String>;
}
