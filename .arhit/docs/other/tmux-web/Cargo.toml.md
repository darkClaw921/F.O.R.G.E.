# tmux-web/Cargo.toml

Cargo manifest для tmux-web. Зависимости: axum 0.7 (ws+macros), tokio 1 (full), tower-http 0.6 (fs+trace), portable-pty 0.8, serde/serde_json, tracing+tracing-subscriber, anyhow, futures-util, bytes, notify 6 (Phase 6.D — file-watcher для .beads/issues.jsonl с tokio mpsc интеграцией). Profile release: opt-level=3, lto=thin.
