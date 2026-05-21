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
use echo_host_api::{HostApi, SessionInfo};

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
}
