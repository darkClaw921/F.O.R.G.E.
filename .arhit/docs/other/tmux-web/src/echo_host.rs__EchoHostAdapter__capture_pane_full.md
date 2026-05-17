# tmux-web/src/echo_host.rs::EchoHostAdapter::capture_pane_full

Phase 3 реальная имплементация HostApi::capture_pane_full. Делегирует напрямую в crate::tmux::capture_pane_full(session, lines), сохраняя контракт: Ok('') если сессия исчезла, Err при невалидном lines, текстовый дамп при успехе.
