# tmux-web — Howto

## Что это

tmux-web — локальный веб-просмотрщик активных tmux-сессий. Бэкенд на Rust + axum + portable-pty, фронтенд — vanilla JS + xterm.js. Слушает только 127.0.0.1, без аутентификации (предполагается доверенная среда).

Возможности:
- Листинг tmux-сессий с автополлингом (3s).
- Создание / kill сессий через UI.
- Полнофункциональный xterm.js терминал, подключённый к выбранной сессии через WebSocket-bridge tmux attach.
- Resize: ResizeObserver на контейнере → SIGWINCH в PTY.
- Switch без переподключения WS: control-сообщение `{"type":"switch","session":"..."}`.
- Параллельные клиенты на одну сессию (родная фича tmux).
- Корректное закрытие WS при внешнем kill-session (через Notify-сигнал EOF из reader-task).

## Как собрать

```bash
cd /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./tmux-web
cargo build --release      # release-сборка → ./target/release/tmux-web
# или
cargo build                # debug-сборка
```

Зависимости системы: tmux в `$PATH`, Rust toolchain (stable, edition 2021).

## Как запустить

```bash
cd /Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./tmux-web
cargo run                  # debug
# или
RUST_LOG=info,tmux_web=debug ./target/release/tmux-web
```

После старта открыть в браузере: <http://127.0.0.1:7331>

Переменные окружения:
- `RUST_LOG` — стандартный фильтр tracing-subscriber. Default: `info,tmux_web=debug`.

## API endpoints

| Метод | Путь | Тело | Ответ | Назначение |
|---|---|---|---|---|
| GET | `/healthz` | — | `200 ok` text | health-check |
| GET | `/api/sessions` | — | `200 [SessionInfo...]` | листинг tmux-сессий (пустой массив если сервер tmux не запущен) |
| POST | `/api/sessions` | `{"name":"<id>"}` | `201 Created` или `400` | создание detached-сессии (`tmux new-session -d -s <id>`) |
| DELETE | `/api/sessions/:name` | — | `204` или `400` | kill сессии (`tmux kill-session -t <name>`) |
| GET | `/ws/attach?session=<n>&cols=<u16>&rows=<u16>` | (upgrade) | WebSocket | bridge клиента в `tmux attach -t <n>` через PTY |

`SessionInfo` (JSON): `{name:string, id:string, attached:u32, windows:u32, created:i64}`.

### WebSocket wire-протокол

- **Binary frames** — сырые байты PTY (xterm-256color escape sequences) в обе стороны: клиент пишет ввод, сервер шлёт stdout.
- **Text frames** от клиента — JSON control:
  - `{"type":"resize","cols":120,"rows":40}` — отправляет SIGWINCH в PTY.
  - `{"type":"switch","session":"other"}` — kill старого PTY, spawn нового на той же WS.
- **Close frame** — корректный teardown (сервер дропает PtyHandle → kill+wait дочерний tmux).

## Архитектура

3 модуля Rust + 3 файла статики:

```
tmux-web/
├── Cargo.toml                    # axum 0.7, tokio 1, portable-pty 0.8, tower-http 0.6, ...
├── src/
│   ├── main.rs                   # точка входа, axum Router, hooked routes
│   ├── tmux.rs                   # CLI-обёртки tmux (list/new/kill), парсинг
│   ├── pty.rs                    # PtyHandle, spawn_tmux_attach (portable-pty)
│   └── ws.rs                     # /ws/attach — handle_socket + spawn_pty_reader
└── static/
    ├── index.html                # layout: sidebar + #terminal + xterm CDN
    ├── app.js                    # xterm init, polling, WS bridge, control msgs
    └── style.css                 # dark-theme layout
```

### Поток данных

```
Browser xterm.js  ⇄  WebSocket (Binary)  ⇄  ws::handle_socket
                                              ↓ ↑ (mpsc 64chunks)
                                              spawn_blocking + std::io::Read/Write
                                              ↓ ↑
                                              PtyHandle (master FD)
                                              ↓ ↑
                                              tmux attach -t <session>
                                              ↓ ↑
                                              tmux server (sessions $0,$1,...)
```

### Ключевые компоненты

- **PtyHandle** (`pty.rs`) — RAII-обёртка над portable-pty. На Drop делает kill+wait дочерней tmux-process.
- **handle_socket** (`ws.rs`) — главная корутина WS-соединения. Запускает 2 параллельных таска (reader, writer) + основной select-loop на ws_rx + pty_eof_notify. Поддерживает switch (swap PtyHandle in-place).
- **pty_eof_notify** (`tokio::sync::Notify`) — сигнал из reader-task о EOF/error, чтобы handler закрыл WS даже если клиент молчит (нужно для внешнего `tmux kill-session`).

## Зависимости

Crates (см. Cargo.toml):
- **axum 0.7** (features: ws, macros) — HTTP+WS веб-фреймворк.
- **tower-http 0.6** (fs, trace) — ServeDir для статики, TraceLayer для логов.
- **tokio 1** (full) — async runtime.
- **portable-pty 0.8** — кросс-платформенный PTY (на macOS posix_openpt/grantpt/unlockpt).
- **serde 1** + **serde_json 1** — JSON I/O.
- **tracing / tracing-subscriber 0.3** — логирование.
- **anyhow 1** — Error type для приложения.
- **futures-util 0.3** — Stream/Sink combinators (split, SinkExt, StreamExt).
- **bytes 1** — Bytes/BytesMut.

Frontend (CDN-only, без npm):
- **xterm.js 5.3.0** + **xterm-addon-fit 0.8.0** + **xterm-addon-web-links 0.9.0** (через cdn.jsdelivr.net).

## Безопасность

- Bind только на 127.0.0.1 — внешний доступ невозможен без обратного proxy.
- Имена сессий валидируются `[A-Za-z0-9_-]+` (см. `tmux::is_valid_session_name`) — защита от инъекций tmux args.
- Все вызовы tmux идут через `tokio::process::Command` args, без shell.

## Verification (Phase 5)

См. /tmp/scenario1..6_ws.py — все 6 сценариев PASS:
1. Нет сессий → `[]` → POST default → 201 → list содержит default. PASS.
2. Несколько сессий: WS к a/b — изолированные потоки. PASS.
3. Resize: cols 80→120, rows 40 (39 видимых из-за status bar tmux). PASS.
4. Switch без reconnect: один WS, switch a→b, разные shell PIDs. PASS.
5. External kill-session: WS закрывается ~1с (после bug-fix forge-qs0). PASS.
6. Параллельные клиенты на одну сессию: оба видят синхронный вывод. PASS.

## История

- **Phase 1**: bootstrap (axum + ServeDir + healthz).
- **Phase 2**: модуль tmux + REST API (`/api/sessions`).
- **Phase 3**: модули pty + ws + `/ws/attach` WebSocket-bridge.
- **Phase 4**: фронтенд (xterm.js + sidebar + control-протокол).
- **Phase 5**: верификация (6 сценариев) + bug-fix forge-qs0 (EOF-detect через Notify) + документация.