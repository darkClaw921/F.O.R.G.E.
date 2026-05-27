//! `EchoHostAdapter` — реализация [`echo_host_api::HostApi`] для F.O.R.G.E.
//!
//! Plugin boundary: `forge-echo` крейт не знает про `AppState`, `tmux::`
//! напрямую. Доступ ко всем хост-ресурсам проходит через trait `HostApi`.
//! Этот файл — единственное место, где импортируются и хост-крейт
//! (`crate::*`), и plugin-API.
//!
//! Реальные impl `list_sessions` (через [`crate::tmux::list_sessions`])
//! и `capture_pane_full` (через [`crate::tmux::capture_pane_full`]).
//!
//! `auth_token()` отдаёт Bearer-токен в remote-mode (`None` в localhost) —
//! Echo WS-клиент сам себя авторизует при self-обращении к хост-API.

use async_trait::async_trait;
use echo_host_api::{HostApi, ProjectActivity, SessionInfo};

use crate::AppState;

/// Адаптер, оборачивающий `AppState` и реализующий [`HostApi`].
///
/// Кладётся в `Arc<dyn HostApi>` и передаётся в `forge_echo::register_routes`
/// и `forge_echo::spawn_workers`. Cheap-clonable (внутри только `Arc`-ы).
pub struct EchoHostAdapter {
    pub state: AppState,
}

#[async_trait]
impl HostApi for EchoHostAdapter {
    /// Возвращает все живые tmux-сессии хоста. Маппит `crate::tmux::SessionInfo`
    /// (расширенная структура хоста с id/attached/created/path/group) в
    /// упрощённый `echo_host_api::SessionInfo`, нужный плагину.
    ///
    /// Если tmux-сервер не запущен — `tmux::list_sessions` отдаст пустой
    /// вектор, мы возвращаем `Ok(vec![])` без ошибки. Это позволяет
    /// prompt-builder'у работать в development-окружении без tmux.
    async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        let host_sessions = crate::tmux::list_sessions().await?;
        let panes_unknown: u32 = 0; // tmux list-sessions не отдаёт суммарный pane-count;
                                    // для prompt-builder'а это не критично — оставляем 0.
        let result = host_sessions
            .into_iter()
            .map(|s| SessionInfo {
                name: s.name,
                windows: s.windows,
                panes: panes_unknown,
            })
            .collect();
        Ok(result)
    }

    /// Делегирует в [`crate::tmux::capture_pane_full`]. Возвращает либо
    /// текстовый дамп pane (включая `lines` строк scrollback), либо пустую
    /// строку если сессия исчезла между listing и capture.
    async fn capture_pane_full(&self, session: &str, lines: i32) -> anyhow::Result<String> {
        crate::tmux::capture_pane_full(session, lines).await
    }

    /// Bearer-token из remote-mode (`None` в localhost-режиме).
    /// `AppState.auth_token: Arc<Option<String>>` — клон Arc дешёвый,
    /// разыменование внутри возвращает `Option<&String>`.
    fn auth_token(&self) -> Option<String> {
        self.state.auth_token.as_ref().clone()
    }

    /// Собирает git-активность с момента `since_unix` по уникальным git-корням
    /// рабочих директорий tmux-сессий.
    ///
    /// Реализован поверх [`collect_project_activity`](Self::collect_project_activity):
    /// берём только проекты с непустым `git_log` и склеиваем их в markdown-блоки
    /// `### <name>` + список коммитов (разделитель — пустая строка). Проекты без
    /// коммитов за день в этот grounding-блок не попадают (важно «что сделано»,
    /// а не «где можно поработать»).
    async fn collect_git_activity(&self, since_unix: i64) -> anyhow::Result<String> {
        let projects = self.collect_project_activity(since_unix).await?;
        let blocks: Vec<String> = projects
            .into_iter()
            .filter(|p| !p.git_log.trim().is_empty())
            .map(|p| format!("### {}\n{}", p.name, p.git_log.trim()))
            .collect();
        Ok(blocks.join("\n\n"))
    }

    /// Собирает активность проектов с момента `since_unix` по уникальным
    /// git-корням рабочих директорий tmux-сессий.
    ///
    /// Алгоритм:
    /// 1. `crate::tmux::list_sessions()` — берём `path` каждой сессии.
    /// 2. Для каждого пути ищем git-корень (`git rev-parse --show-toplevel`),
    ///    дедуплицируем корни (одна репа может быть открыта в нескольких сессиях).
    /// 3. Для каждого уникального корня — `git log --since=<unix> --pretty=...`;
    ///    формируем [`ProjectActivity`] { path: root, name: basename, git_log }.
    /// 4. Проект включается даже с пустым `git_log` (активен в сессии — кандидат
    ///    на задачи). Не-git каталоги и ошибки отдельных репозиториев тихо
    ///    пропускаются. Если сессий нет — пустой вектор.
    async fn collect_project_activity(
        &self,
        since_unix: i64,
    ) -> anyhow::Result<Vec<ProjectActivity>> {
        use std::collections::BTreeSet;
        use tokio::process::Command;

        let sessions = match crate::tmux::list_sessions().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "collect_project_activity: list_sessions failed");
                return Ok(Vec::new());
            }
        };

        // Уникальные пути сессий (несколько сессий могут смотреть в один cwd).
        let mut seen_paths: BTreeSet<String> = BTreeSet::new();
        let mut roots: BTreeSet<String> = BTreeSet::new();
        for s in sessions {
            if s.path.trim().is_empty() || !seen_paths.insert(s.path.clone()) {
                continue;
            }
            // git-корень для пути. Не-репозитории дают ненулевой exit — пропускаем.
            let out = Command::new("git")
                .args(["-C", &s.path, "rev-parse", "--show-toplevel"])
                .output()
                .await;
            if let Ok(out) = out {
                if out.status.success() {
                    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !root.is_empty() {
                        roots.insert(root);
                    }
                }
            }
        }

        let since_arg = format!("@{since_unix}"); // unix timestamp понятен git --since.
        let mut projects: Vec<ProjectActivity> = Vec::new();
        for root in roots {
            // git_log может быть пустым — проект всё равно включаем (кандидат).
            let git_log = match Command::new("git")
                .args([
                    "-C",
                    &root,
                    "log",
                    &format!("--since={since_arg}"),
                    "--pretty=format:- %h %s",
                ])
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    String::from_utf8_lossy(&out.stdout).trim().to_string()
                }
                _ => String::new(),
            };
            let name = std::path::Path::new(&root)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone());
            projects.push(ProjectActivity {
                path: root,
                name,
                git_log,
            });
        }

        Ok(projects)
    }
}
