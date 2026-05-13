# tmux-web

> Веб-интерфейс для активных tmux-сессий. Rust + Axum + WebSocket + xterm.js. с поддержкой работы с .beads

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust)](https://www.rust-lang.org)
[![Axum](https://img.shields.io/badge/axum-0.7-blueviolet)](https://github.com/tokio-rs/axum)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-active-success)]()

FORGE  Flow Orchestration and Real-time Governance Engine

---

## ✨ Возможности

- 🖥️ **Полноценный терминал в браузере** — xterm.js с поддержкой `xterm-256color`.
- 🔌 **WebSocket-bridge** между браузером и PTY с реальным `tmux attach`.
- 📋 **Список сессий** в реальном времени: имя, окна, клиенты, время создания.
- ➕ **Создание / удаление** сессий через REST API (`POST` / `DELETE /api/sessions`).
- 🔄 **Hot-switch** между сессиями без перезагрузки страницы.
- 📐 **Авто-resize** PTY при изменении размера окна браузера.
- 🪶 **Один бинарь** — без Node, без Docker, без зависимостей в рантайме (кроме `tmux`).

---

## 🏗️ Архитектура

```
┌──────────────┐   WS (binary + JSON)   ┌──────────────┐   PTY    ┌──────────┐
│  Browser     │ ◀──────────────────▶  │  axum + tokio │ ◀──────▶ │  tmux    │
│  xterm.js    │   xterm escape seq    │  Rust server  │  bytes   │  attach  │
└──────────────┘                        └──────────────┘          └──────────┘
       │                                       │
       │  HTTP REST                            │
       │  /api/sessions, /healthz              │
       └──────────────────────────────────────▶│
```

| Модуль    | Назначение                                                                  |
| --------------- | ------------------------------------------------------------------------------------- |
| `src/main.rs` | Axum-роутер, статика, health-check, REST endpoints.                      |
| `src/tmux.rs` | Обёртка над `tmux` CLI: list / new / kill сессий.                   |
| `src/pty.rs`  | `portable-pty` + `tmux attach` для одной WS-сессии.                 |
| `src/ws.rs`   | WebSocket bridge — байты в обе стороны + control-сообщения. |
| `static/`     | `index.html`, `app.js`, `style.css` — фронтенд.                        |

---

## 🚀 Быстрый старт

### Требования

- **Rust** 1.75+
- **tmux** в `$PATH`
- **macOS / Linux** (Windows не поддерживается из-за PTY)

### Запуск

```bash
git clone <repo>
cd tmux-web
cargo run --release
```

Открыть в браузере: **http://127.0.0.1:7331**

### Кастомное логирование

```bash
RUST_LOG=tmux_web=trace,tower_http=debug cargo run --release
```

---

## 📡 REST API

| Method | Path                    | Описание                     | Ответ              |
| ------ | ----------------------- | ------------------------------------ | ----------------------- |
| GET    | `/healthz`            | Health-check                         | `200 ok`              |
| GET    | `/api/sessions`       | Список tmux-сессий       | `200 [SessionInfo]`   |
| POST   | `/api/sessions`       | Создать detached-сессию | `201 Created`         |
| DELETE | `/api/sessions/:name` | Убить сессию              | `204 No Content`      |
| GET    | `/ws/attach`          | WebSocket attach (см. ниже)    | `101 Switching Proto` |

### `SessionInfo`

```json
{
  "name": "main",
  "id": "$0",
  "attached": 1,
  "windows": 3,
  "created": 1714900000
}
```

### `POST /api/sessions`

```json
{ "name": "dev" }
```

---

## 🔌 WebSocket-протокол

**URL:** `ws://127.0.0.1:7331/ws/attach?session=<name>&cols=<u16>&rows=<u16>`

| Frame  | Направление | Содержимое                                                                        |
| ------ | ---------------------- | ------------------------------------------------------------------------------------------- |
| Binary | Both                   | Сырые байты PTY (`xterm-256color` escape-последовательности). |
| Text   | Client → Server       | JSON control:`resize`, `switch`.                                                        |
| Close  | Both                   | Корректный teardown — kill PTY + wait child.                                     |

### Control-сообщения

```jsonc
// Изменить размер PTY
{ "type": "resize", "cols": 120, "rows": 40 }

// Переключиться на другую сессию (kill old PTY, spawn new)
{ "type": "switch", "session": "other" }
```

Невалидный JSON или неизвестный `type` → лог `warn`, соединение остаётся живым.

---

## 🧩 Конфигурация

Сейчас всё прибито гвоздями к `127.0.0.1:7331`. Параметры окружения:

| Переменная | Назначение                                | По умолчанию |
| -------------------- | --------------------------------------------------- | ----------------------- |
| `RUST_LOG`         | Уровень логирования (`tracing`) | `info,tmux_web=debug` |

Каталог `static/` ищется:

1. `./static` относительно cwd.
2. Рядом с бинарём (`<exe>/../static`).

---

## 🛠️ Разработка

```bash
# Проверка компиляции
cargo check

# Линтер
cargo clippy -- -D warnings

# Форматирование
cargo fmt

# Тесты
cargo test
```

---

## 📦 Стек

- [`axum`](https://github.com/tokio-rs/axum) 0.7 — HTTP + WebSocket.
- [`tokio`](https://tokio.rs) — async runtime.
- [`portable-pty`](https://crates.io/crates/portable-pty) — PTY-обёртка.
- [`tower-http`](https://crates.io/crates/tower-http) — `ServeDir` + tracing.
- [`xterm.js`](https://xtermjs.org) 5.3 — терминал в браузере.

---

## 📜 Лицензия

MIT © tmux-web contributors
