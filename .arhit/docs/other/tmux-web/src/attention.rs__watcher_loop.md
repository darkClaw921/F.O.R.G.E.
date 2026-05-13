# tmux-web/src/attention.rs::watcher_loop

Фоновый watcher: каждые 1500ms обходит все tmux-сессии. Для каждой: capture_pane → detect_claude_prompt → attention.set(name, flag). Диагностическое логирование (tracing::debug!): session, group (Option<String>), pane_hash (u64 от DefaultHasher по содержимому pane), detected, pane_len. Видно при RUST_LOG=tmux_web=debug. pane_hash используется для дедупликации (Phase 1.3). Loop никогда не завершается; ошибки tmux::list_sessions/capture_pane не валят loop (unwrap_or_default).
