# README

Корневой README.md проекта F.O.R.G.E. (devforge). Главный документ для пользователей и контрибьюторов.

Структура (top-down):
- Заголовок-баннер с logo (assets/logo.svg), badges (Rust, Axum, xterm.js, Claude Code, MIT, status).
- Навигационная панель с якорными ссылками.
- '📜 Что это' — краткое описание (tmux+канбан+git+AI-агенты в одном веб-окне, один Rust-бинарь).
- '🖼️ Скриншоты' — таблица из assets/screenshots/{terminal,tasks,git}.png.
- '✨ Возможности' — фичи по разделам: Terminal & tmux, Tasks (Kanban на beads), TODO-конвейер, Git (lazygit), Projects, Темы, Notifications, UX.
- '🧠 Claude Code-интеграция' — заточенность под Anthropic CLI.
- '🏗️ Архитектура' — ASCII-диаграмма + таблица модулей.
- '🚀 Быстрый старт' — содержит:
  - '🍺 Установка через Homebrew (macOS)' (Phase 4): команды brew tap darkClaw921/devforge / brew install devforge / devforge, флаги --help / --version, runtime-зависимости (tmux обязательно, lazygit/br опционально), ссылка на docs/homebrew-tap-setup.md.
  - 'Требования (сборка из исходников)': Rust 1.75+, tmux, macOS/Linux.
  - 'Сборка и запуск': git clone + cargo run --release, открыть http://127.0.0.1:7331.
  - Логирование (RUST_LOG), Init проекта в UI.
- '📡 REST API' — таблица всех endpoints (/api/sessions, /api/tasks, /api/projects, /api/todos, /api/themes, /ws/*).
- '🔌 WebSocket-протокол' — описание фреймов и JSON-control.
- '🧩 Конфигурация' — env vars, state-файлы (.beads, .forge, themes.json, projects.json).
- '🛠️ Разработка' — cargo check/clippy/fmt/test, workflow с Claude Code (br ready / arhit context / br close / br sync).
- '📦 Стек' — список зависимостей со ссылками.
- '🗺️ Roadmap' — чек-лист сделанного и план.
- '📜 Лицензия' — MIT.

Phase 4 изменение: добавлена секция '🍺 Установка через Homebrew (macOS)' в начале раздела '🚀 Быстрый старт', сразу перед 'Требования (сборка из исходников)'. Стиль соответствует соседним секциям (эмодзи в заголовке, code-блоки bash, маркированные списки runtime-зависимостей).
