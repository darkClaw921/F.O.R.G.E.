<div align="center">

<img src="assets/logo.svg" alt="F.O.R.G.E. — Flow Orchestration and Real-time Governance Engine" width="720"/>

<h1 align="center">F.O.R.G.E.</h1>

<p align="center"><em>Flow Orchestration and Real-time Governance Engine</em></p>

<p align="center"><strong>Веб-кокпит для разработчика: tmux + канбан + git + AI-агенты в одном окне.</strong></p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust&logoColor=white" alt="Rust"/></a>
  <a href="https://github.com/tokio-rs/axum"><img src="https://img.shields.io/badge/axum-0.7-blueviolet" alt="Axum"/></a>
  <a href="https://xtermjs.org"><img src="https://img.shields.io/badge/xterm.js-5.3-black?logo=javascript" alt="xterm.js"/></a>
  <a href="https://docs.anthropic.com/en/docs/claude-code"><img src="https://img.shields.io/badge/Claude%20Code-Opus%204.7-D97757?logo=anthropic&logoColor=white" alt="Claude Code"/></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green.svg" alt="License"/></a>
  <img src="https://img.shields.io/badge/status-active-success" alt="Status"/>
</p>

<p align="center">
  <a href="#-возможности">Возможности</a> ·
  <a href="#-архитектура">Архитектура</a> ·
  <a href="#-быстрый-старт">Старт</a> ·
  <a href="#-claude-code-интеграция">Claude</a> ·
  <a href="#-rest-api">API</a>
</p>

</div>

---

## 📜 Что это

**F.O.R.G.E.** — единый рабочий стол для разработчика, который держит в одной вкладке браузера всё, ради чего раньше держал четыре терминала и три приложения:

- живые **tmux-сессии** через WebSocket + xterm.js,
- **канбан-доски** на базе [beads_rust](https://github.com/Dicklesworthstone/beads_rust),
- **lazygit** прямо в браузерной вкладке,
- **TODO**-конвейер, прокидывающий задачи прямо в tmux-сессию,
- **AI-агенты** Claude Code (декомпозиция плана, выполнение фаз, трекинг времени),
- мульти-проектность, темы, нотификации.

Один Rust-бинарь, без Node, без Docker.

---

## 🖼️ Скриншоты

<div align="center">

<table>
  <tr>
    <td align="center" width="50%">
      <img src="assets/screenshots/terminal.png" alt="Terminal — tmux в браузере" width="100%"/>
      <br/>
      <sub><b>🖥️ Terminal</b> — tmux-сессии через xterm.js + WebSocket</sub>
    </td>
    <td align="center" width="50%">
      <img src="assets/screenshots/tasks.png" alt="Tasks — канбан на beads" width="100%"/>
      <br/>
      <sub><b>📋 Tasks</b> — канбан-доска на beads_rust с live-стримом</sub>
    </td>
  </tr>
  <tr>
    <td colspan="2" align="center">
      <img src="assets/screenshots/git.png" alt="Git — lazygit в браузере" width="80%"/>
      <br/>
      <sub><b>🌿 Git</b> — встроенный lazygit через PTY/WebSocket</sub>
    </td>
  </tr>
</table>

</div>

---

## ✨ Возможности

### 🖥️ Terminal & tmux

- **Полноценный tmux в браузере** — xterm.js + WebSocket-bridge, `xterm-256color`, true-color.
- **Список сессий** в реальном времени: имя, окна, клиенты, время создания.
- **Создание / удаление / hot-switch** сессий без перезагрузки страницы.
- **Авто-resize PTY** при изменении размера окна.
- **Выбор папки запуска** новой tmux-сессии (произвольный cwd).
- Один бинарь, рантайм-зависимость только `tmux`.

### 📋 Tasks (Kanban на beads)

- Канбан-доска на каждый проект, статусы из `br` (beads).
- WebSocket-стрим `/ws/tasks` — изменения в `.beads/issues.jsonl` прилетают в UI мгновенно (file-watcher через `notify`).
- Создание / закрытие / reopen / patch — прямо из UI, ходит через REST в `br`.
- Приоритеты P0–P4, типы (task / bug / feature / epic / chore / docs / question), зависимости.

### ✅ TODO-конвейер

- Стадии: **inbox → нужно сделать → сделано**.
- Промоут TODO → tmux-сессия: при переводе задачи в активную стадию текст уходит **в нужную tmux-сессию проекта** как промпт.
- Настройка отправки: сразу или после завершения предыдущей TODO в этом проекте.
- WebSocket-стрим `/ws/todos`.

### 🌿 Git (lazygit)

- Вкладка **Git** — встроенный `lazygit` через тот же PTY/WS-механизм (`/ws/lazygit`).
- Авто-определение установки + подсказка по установке на macOS/Linux при отсутствии.

### 📁 Projects

- Несколько проектов, каждый со своим `.beads/`, `.forge/`, темой, настройками.
- `POST /api/projects/init` — инициализация: создаёт `CLAUDE.md`, `TODO.md`, `.gitignore`, делает `git init`.
- Активный проект сохраняется на сервере, переключение в один клик.

### 🎨 Темы (9 пресетов + custom)

`Default · Dracula · Solarized Dark · Solarized Light · Monokai · Nord · Gruvbox Dark · One Dark · Tokyo Night`

- Единая модель `Theme { ui, term }` — красит и UI, и xterm.js.
- Пользовательские темы (CRUD `/api/themes/custom`), атомарное сохранение в `themes.json`.

### 🔔 Notifications & Attention

- `attention.rs` — детектор «требует внимания» в tmux-сессии (ANSI bell, кастомные триггеры).
- `notifier.rs` — десктоп-нотификации с шаблонизатором сообщений.
- Состояние в `.forge/notify_state.json`.

### ⌨️ UX

- Vim-style hotkeys + Cmd-hint mode (`hotkeys.js`).
- Sidebar + табы (Terminal / Tasks / Git), полностью клавиатурный воркфлоу.
- Status-dot WS-соединения, авто-reconnect.

---

## 🧠 Claude Code-интеграция

Проект заточен под совместную работу с **Claude Code** (Anthropic CLI).

## 🏗️ Архитектура

```
┌──────────────────────────────────────────────────────────────────────┐
│  Browser (xterm.js + app.js + hotkeys.js)                            │
│  Sidebar │ Terminal │ Tasks (Kanban) │ Git (lazygit) │ Themes        │
└─────┬──────────────────┬─────────────────┬─────────────────┬─────────┘
      │ WS /ws/attach    │ WS /ws/lazygit  │ WS /ws/tasks    │ WS /ws/todos
      │ (binary PTY)     │ (binary PTY)    │ (JSON events)   │ (JSON events)
┌─────▼──────────────────▼─────────────────▼─────────────────▼─────────┐
│  Rust server (axum 0.7 + tokio)                                      │
│  REST: /api/sessions /api/tasks /api/todos /api/projects /api/themes │
│                                                                      │
│  pty.rs ── portable-pty ── tmux attach / lazygit                     │
│  tasks.rs ── shells out to `br` (beads_rust)                         │
│  tasks_watcher.rs ── notify(6) ── .beads/issues.jsonl                │
│  projects.rs ── .forge/projects.json + git init scaffolding          │
│  themes.rs   ── 9 presets + custom (atomic write)                    │
│  attention.rs + notifier.rs ── desktop notifications                 │
└──────────────────────────────────────────────────────────────────────┘
```

| Модуль                               | Назначение                                                             |
| ------------------------------------------ | -------------------------------------------------------------------------------- |
| `src/main.rs`                            | Axum-роутер, REST endpoints, статика, health-check.                 |
| `src/tmux.rs`                            | Обёртка над `tmux` CLI: list / new / kill.                           |
| `src/pty.rs`                             | `portable-pty` + `tmux attach` / `lazygit` на одну WS-сессию.  |
| `src/ws.rs`                              | WebSocket-bridge: байты PTY ↔ браузер + control-сообщения. |
| `src/tasks.rs`                           | REST `/api/tasks`, дёргает `br`.                                      |
| `src/ws_tasks.rs` + `tasks_watcher.rs` | WS-стрим изменений `.beads/issues.jsonl`.                        |
| `src/todos.rs` + `ws_todos.rs`         | TODO-конвейер с прокидыванием в tmux.                     |
| `src/projects.rs`                        | Мульти-проекты, init, активный проект.                |
| `src/themes.rs`                          | 9 пресетов + custom темы.                                            |
| `src/attention.rs` + `notifier.rs`     | Десктоп-нотификации.                                           |
| `static/`                                | `index.html`, `app.js` (~4.5k), `style.css` (~2.2k), `hotkeys.js`.       |

---

## 🚀 Быстрый старт

### Требования

- **Rust** 1.75+
- **tmux** в `$PATH`
- **macOS / Linux** (Windows не поддерживается из-за PTY)
- *(опционально)* `lazygit` для git-вкладки
- *(опционально)* `br` ([beads_rust](https://github.com/Dicklesworthstone/beads_rust)) для канбана
- *(опционально)* Claude Code CLI для AI-агентов

### Сборка и запуск

```bash
git clone <repo> F.O.R.G.E.
cd F.O.R.G.E./tmux-web
cargo run --release
```

Открыть: **http://127.0.0.1:7331**

### Логирование

```bash
RUST_LOG=tmux_web=trace,tower_http=debug cargo run --release
```

### Инициализация нового проекта в UI

`Sidebar → ⚙ Settings → New project → Init`. Создаёт `CLAUDE.md`, `TODO.md`, `.gitignore`, делает `git init`.

---

## 📡 REST API

| Method                      | Path                                | Описание                                   |
| --------------------------- | ----------------------------------- | -------------------------------------------------- |
| GET                         | `/healthz`                        | Health-check.                                      |
| GET / POST                  | `/api/sessions`                   | Список / создание tmux-сессий. |
| DELETE                      | `/api/sessions/:name`             | Убить сессию.                           |
| GET / POST                  | `/api/tasks`                      | Канбан-задачи (`br`).                |
| PATCH / DELETE              | `/api/tasks/:id`                  | Обновить / закрыть задачу.    |
| POST                        | `/api/tasks/:id/reopen`           | Переоткрыть закрытую.           |
| GET / POST                  | `/api/projects`                   | Проекты.                                    |
| PATCH                       | `/api/projects/:id/settings`      | Настройки проекта.                 |
| POST                        | `/api/projects/active`            | Сделать активным.                   |
| POST                        | `/api/projects/init`              | Init (CLAUDE.md, TODO.md, git).                    |
| GET / POST / PATCH / DELETE | `/api/todos[/:id]`                | TODO CRUD.                                         |
| POST                        | `/api/todos/:id/promote`          | Промоут TODO → tmux.                       |
| GET / PATCH                 | `/api/themes[/active]`            | Темы.                                          |
| POST / PUT / DELETE         | `/api/themes/custom[/:id]`        | Custom-темы.                                   |
| GET (WS)                    | `/ws/attach?session=&cols=&rows=` | tmux attach.                                       |
| GET (WS)                    | `/ws/lazygit?project=`            | lazygit.                                           |
| GET (WS)                    | `/ws/tasks` / `/ws/todos`       | Live-стримы JSON.                            |

---

## 🔌 WebSocket-протокол (terminal)

`ws://127.0.0.1:7331/ws/attach?session=<name>&cols=<u16>&rows=<u16>`

| Frame  | Направление | Содержимое                            |
| ------ | ---------------------- | ----------------------------------------------- |
| Binary | Both                   | Сырые байты PTY (`xterm-256color`). |
| Text   | Client → Server       | JSON control:`resize`, `switch`.            |
| Close  | Both                   | Teardown: kill PTY + wait child.                |

```jsonc
{ "type": "resize", "cols": 120, "rows": 40 }
{ "type": "switch", "session": "other" }
```

---

## 🧩 Конфигурация

| Переменная | Назначение                                | По умолчанию |
| -------------------- | --------------------------------------------------- | ----------------------- |
| `RUST_LOG`         | Уровень логирования (`tracing`) | `info,tmux_web=debug` |

Привязка: `127.0.0.1:7331` (hardcoded на текущей фазе).

Каталог `static/` ищется: `./static` → `<exe>/../static`.

State-файлы:

- `.beads/issues.jsonl` — задачи (под git).
- `.forge/todos.json` — TODO-стейт.
- `.forge/notify_state.json` — нотификации.
- `<data_dir>/themes.json` — темы.
- `<data_dir>/projects.json` — проекты.

---

## 🛠️ Разработка

```bash
cargo check
cargo clippy -- -D warnings
cargo fmt
cargo test
```

### Workflow с Claude Code

```bash
# Начало сессии
br ready                       # доступные задачи
arhit context                  # контекст проекта

# Работа
br update <id> --status=in_progress
# ... код + arhit doc add <el> --content "..."
br close <id>

# Конец сессии
br sync --flush-only
git status && git push
```

---

## 📦 Стек

- [`axum`](https://github.com/tokio-rs/axum) 0.7 — HTTP + WebSocket
- [`tokio`](https://tokio.rs) — async runtime
- [`portable-pty`](https://crates.io/crates/portable-pty) 0.8 — PTY
- [`tower-http`](https://crates.io/crates/tower-http) 0.6 — `ServeDir` + tracing
- [`notify`](https://crates.io/crates/notify) 6 — file-watcher `.beads`
- [`tracing`](https://crates.io/crates/tracing) — структурный лог
- [`xterm.js`](https://xtermjs.org) 5.3 + `fit` + `web-links` — терминал
- [`beads_rust`](https://github.com/Dicklesworthstone/beads_rust) (`br`) — issue tracker
- [`lazygit`](https://github.com/jesseduffield/lazygit) — git UI
- [`arhit`](https://github.com/) — архитектурная документация
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) — AI-агенты

---

## 🗺️ Roadmap

- [X] tmux-сессии в браузере (WS + xterm.js)
- [X] Канбан на beads + file-watcher
- [X] TODO-конвейер с прокидыванием в tmux
- [X] Lazygit-таб
- [X] Мульти-проекты + init
- [X] 9 тем + custom
- [X] Sub-agents (create-tasks / run-phase / time-tracker)
- [ ] Touch DnD (мобилки)
- [ ] BB/RBAC, multi-user
- [ ] Поиск/фильтр по labels/assignee в Tasks
- [ ] Графики/burndown
- [X] Прокидывание задач в tmux текстом (см. `TODO.md` prompt 4)

---

## 📜 Лицензия

MIT © F.O.R.G.E. contributors
