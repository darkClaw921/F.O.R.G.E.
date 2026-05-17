# /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./Cargo.toml

Cargo workspace root для F.O.R.G.E. Определяет три члена workspace: tmux-web (основной axum-бинарь devforge), plugins/echo (плагин Echo чат на Claude CLI), plugins/echo-host-api (мини-крейт с trait HostApi и DTO для plugin boundary). Исключает beads_rust (не должен конфликтовать с workspace). Содержит [workspace.dependencies] для версионирования общих deps: tokio, serde, serde_json, anyhow, tracing, axum, chrono, futures-util, async-trait. resolver=2 обязательно для axum 0.7 и rusqlite bundled.
