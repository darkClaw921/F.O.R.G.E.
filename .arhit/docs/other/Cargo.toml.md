# Cargo.toml

Манифест Rust-пакета devforge (бинарь tmux-web/devforge). 

## Зависимости
- **axum 0.7** — HTTP/WS-сервер с features ws, macros.
- **tokio 1** (full) — async-runtime.
- **tower-http 0.6** — middleware (fs, trace).
- **portable-pty 0.8** — кросс-платформенный PTY для tmux/lazygit.
- **serde + serde_json** — сериализация DTO.
- **tracing + tracing-subscriber** — структурное логирование.
- **anyhow** — error-helpers.
- **futures-util 0.3** — StreamExt/SinkExt для async-итерации.
- **bytes 1** — буферы для proxy body.
- **notify 6** — file-watcher для .beads/issues.jsonl (Phase 6.D).
- **uuid 1.23** (v4) — генерация id.
- **rust-embed 8** (interpolate-folder-path) — встраивание static/ в бинарь (Phase 1 Embedded).
- **mime_guess 2** — Content-Type для embedded ассетов.
- **reqwest 0.12** (json, stream, rustls-tls; default-features=false) — HTTP-клиент для proxy в Phase 3.
- **tokio-tungstenite 0.24** (connect, handshake, rustls-tls-webpki-roots) — WS-клиент для proxy в Phase 4 (proxy_websocket для /ws/attach, /ws/lazygit, /ws/tasks, /ws/todos).
- **libc 0.2** (target=unix) — setsid(2) для daemon-режима.

## Профили
- **release**: opt-level=3, lto=thin.

## dev-dependencies
- **tower 0.5** (util) — ServiceExt::oneshot для тестов auth.rs.

## bin
- name=devforge, path=src/main.rs.

## Метаданные
name=devforge, version=0.1.3, edition=2021, license=MIT, repo darkClaw921/F.O.R.G.E.
