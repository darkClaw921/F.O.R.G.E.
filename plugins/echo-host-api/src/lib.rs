//! Plugin boundary для Echo плагина.
//!
//! Этот крейт определяет [`HostApi`] — trait, через который Echo получает
//! доступ к хост-системе (tmux, auth). Конкретная реализация
//! живёт в `tmux-web/src/echo_host.rs` (`EchoHostAdapter`), которая
//! оборачивает `Arc<AppState>` без утечки его в плагин.
//!
//! После Phase 4 (`remove-projects-concept`) host-API больше не выдаёт
//! «проекты» — концепция удалена из F.O.R.G.E. целиком. Echo продолжает
//! хранить опциональный `project_id` в своих SQLite-таблицах как
//! непрозрачный soft-FK-строковый ярлык: callers могут передавать туда
//! любые идентификаторы (например, путь корня), но валидация и
//! перечисление снаружи плагина не предоставляются.

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

/// Активность одного проекта (git-репозитория) за период.
///
/// Используется генерацией «Сводки дня» для раздела «предлагаемые задачи по
/// проектам»: каждый уникальный git-корень рабочих директорий tmux-сессий
/// рассматривается как отдельный проект-кандидат на постановку задач.
///
/// # Поля
///
/// - `path` — git-корень репозитория (абсолютный путь). Используется как
///   `path` при создании TODO, поэтому это стабильный ключ проекта.
/// - `name` — basename корня для отображения в UI.
/// - `git_log` — коммиты репозитория с начала дня (markdown-список `- %h %s`).
///   Может быть пустым: проект активен в сессии (кандидат на задачи), даже
///   если за день в нём не было коммитов.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectActivity {
    /// Git-корень (используется как `path` при создании TODO).
    pub path: String,
    /// Basename корня для отображения.
    pub name: String,
    /// Коммиты с начала дня (может быть пустым).
    pub git_log: String,
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
/// - [`auth_token`](HostApi::auth_token) — Bearer-token в remote-mode
///   (`None` в localhost-режиме). Echo использует его для WS-протокола,
///   когда плагин сам обращается к хост-API.
#[async_trait]
pub trait HostApi: Send + Sync {
    async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>>;

    async fn capture_pane_full(&self, session: &str, lines: i32) -> anyhow::Result<String>;

    fn auth_token(&self) -> Option<String>;

    /// Собирает git-активность хоста с момента `since_unix` (unix seconds) —
    /// markdown-блок коммитов по уникальным git-корням рабочих директорий
    /// сессий. Используется генерацией «Сводки дня» для grounding раздела
    /// «Что сделано».
    ///
    /// # Контракт
    ///
    /// - Реализация обходит уникальные git-корни (например, из путей сессий),
    ///   для каждого выполняет `git log --since=<since>` и склеивает результат
    ///   в markdown.
    /// - Не-git каталоги и ошибки отдельных репозиториев тихо пропускаются.
    /// - Пустой результат (нет коммитов / нет репозиториев) — `Ok(String::new())`.
    ///
    /// Default-реализация возвращает пустую строку, чтобы тестовые stub'ы и
    /// прочие impl'ы не ломались — ровно тот же контракт, что у «нет активности».
    async fn collect_git_activity(&self, since_unix: i64) -> anyhow::Result<String> {
        let _ = since_unix;
        Ok(String::new())
    }

    /// Собирает активность проектов хоста с момента `since_unix` (unix seconds).
    ///
    /// В отличие от [`collect_git_activity`](HostApi::collect_git_activity)
    /// (который склеивает всё в единый markdown-блок для grounding раздела
    /// «Что сделано»), этот метод возвращает структурированный список
    /// [`ProjectActivity`] — по одному на уникальный git-корень рабочих
    /// директорий сессий. Используется генерацией предложений задач по
    /// проектам.
    ///
    /// # Контракт
    ///
    /// - Один элемент на уникальный git-корень среди путей tmux-сессий.
    /// - Проект включается даже с пустым `git_log` (активен в сессии).
    /// - Если сессий нет — пустой вектор.
    /// - Не-git каталоги и ошибки отдельных репозиториев тихо пропускаются.
    ///
    /// Default-реализация возвращает пустой вектор, чтобы тестовые stub'ы и
    /// прочие impl'ы не ломались.
    async fn collect_project_activity(
        &self,
        since_unix: i64,
    ) -> anyhow::Result<Vec<ProjectActivity>> {
        let _ = since_unix;
        Ok(Vec::new())
    }
}
