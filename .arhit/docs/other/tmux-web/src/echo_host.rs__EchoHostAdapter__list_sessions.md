# tmux-web/src/echo_host.rs::EchoHostAdapter::list_sessions

Phase 3 реальная имплементация HostApi::list_sessions. Делегирует в crate::tmux::list_sessions, маппит расширенную хостовую SessionInfo (id/attached/created/path/group) в упрощённую echo_host_api::SessionInfo (name/windows/panes). Field panes возвращается как 0 — tmux list-sessions не отдаёт суммарный pane-count, для prompt-builder'а это не критично. При non-running tmux-server возвращает пустой вектор.
