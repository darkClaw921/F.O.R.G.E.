//! `EchoHostAdapter` — реализация [`echo_host_api::HostApi`] для F.O.R.G.E.
//!
//! Plugin boundary: `forge-echo` крейт не знает про `AppState`, `tmux::`,
//! `projects::ProjectStore` напрямую. Доступ ко всем хост-ресурсам проходит
//! через trait `HostApi`. Этот файл — единственное место, где импортируются
//! и хост-крейт (`crate::*`), и plugin-API.
//!
//! Phase 3: реальные impl `list_sessions` (через [`crate::tmux::list_sessions`])
//! и `capture_pane_full` (через [`crate::tmux::capture_pane_full`]).
//! `list_projects` / `active_project_id` / `auth_token` подключены ещё в Phase 1.
//!
//! `auth_token()` отдаёт Bearer-токен в remote-mode (`None` в localhost) —
//! Echo WS-клиент сам себя авторизует при self-обращении к хост-API.

use async_trait::async_trait;
use echo_host_api::{HostApi, ProjectInfo, SessionInfo};

use crate::AppState;

/// Адаптер, оборачивающий `AppState` и реализующий [`HostApi`].
///
/// Кладётся в `Arc<dyn HostApi>` и передаётся в `forge_echo::register_routes`
/// и `forge_echo::spawn_workers`. Cheap-clonable (внутри только `Arc`-ы).
//
// `dead_code` будет снят в P1.7 когда EchoHostAdapter::new() вызовется
// в main() после `forge_echo::init`. Сам файл подключён через `mod echo_host`.
#[allow(dead_code)]
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

    /// Список проектов из `ProjectStore`. Уже рабочая реализация — Phase 1
    /// безопасно отдаёт реальные данные, потому что эндпоинт `/api/echo/healthz`
    /// (единственный в P1) её не дёргает; но это позволяет в Phase 2 сразу
    /// валидировать soft-FK `project_id` без дополнительных правок.
    async fn list_projects(&self) -> anyhow::Result<Vec<ProjectInfo>> {
        let store = self.state.projects.read().await;
        let result = store
            .list()
            .into_iter()
            .map(|p| ProjectInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                path: p.path.to_string_lossy().into_owned(),
            })
            .collect();
        Ok(result)
    }

    /// Id активного проекта (или transient_active, если установлен).
    /// Возвращает `Some` всегда — `ProjectStore::active()` гарантирует
    /// наличие активного проекта. `None` зарезервирован на будущее, если
    /// появится «no active project» состояние.
    async fn active_project_id(&self) -> Option<String> {
        let store = self.state.projects.read().await;
        Some(store.active_id().to_string())
    }

    /// Bearer-token из remote-mode (`None` в localhost-режиме).
    /// `AppState.auth_token: Arc<Option<String>>` — клон Arc дешёвый,
    /// разыменование внутри возвращает `Option<&String>`.
    fn auth_token(&self) -> Option<String> {
        self.state.auth_token.as_ref().clone()
    }
}
