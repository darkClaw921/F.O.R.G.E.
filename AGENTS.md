- при использовании любых зависимостей используй MCP context7-mcp он покажет актуальную информацию
- никогда не исаользуй команды git add и git commit
- если что-то правиш в коде проверяй изменения на синтаксис и ошибки компиляции
- используй скиллы ниже чтобы делать задачи и выполнять их
- для создания и выполения задч всегда используй суб-агентов ниже
- мы некогда не запускаем локальные llm модели на cpu только gpu и msp

## Cargo Workspace

С Phase 1 Echo проект — это Cargo workspace в корне `/Users/igorgerasimov/claudeWorkspace/F.O.R.G.E./`:

```
F.O.R.G.E./
├── Cargo.toml                    # workspace root, members + общие deps
├── tmux-web/                     # бинарь devforge (axum + tmux + WS)
└── plugins/
    ├── echo/                     # плагин Echo (forge-echo): чат на Codex CLI
    └── echo-host-api/            # plugin boundary (trait HostApi + DTO)
```

`beads_rust/` исключён из workspace.

### Сборка

```bash
cargo build -p devforge           # основной бинарь
cargo build -p forge-echo         # плагин Echo
cargo build -p echo-host-api      # plugin boundary
cargo build --workspace           # всё сразу
cargo test -p forge-echo          # тесты плагина (Phase 2+)
```

### Plugin boundary

Echo НЕ зависит от `AppState` напрямую — все хост-вызовы идут через
trait `echo_host_api::HostApi`. Конкретная реализация —
`tmux-web/src/echo_host.rs::EchoHostAdapter`. Lifecycle:
`forge_echo::init` → `forge_echo::register_routes` (вызывается ДО
`.layer(auth_middleware)` в `main.rs`) → `forge_echo::spawn_workers`
(Phase 4+).

Phase 1 даёт только `GET /api/echo/healthz` (smoke endpoint).

## Sub-Agents

Проект использует специализированных суб-агентов, запускаемых через Task tool. Каждый суб-агент — это отдельный процесс с собственным контекстом и набором инструментов.

### create-tasks

**Тип:** `subagent_type: "create-tasks"`

Агент декомпозиции планов. Принимает утверждённый план реализации и автономно создаёт структурированные задачи в br (beads issue tracker).

- **Триггер:** Пользователь одобрил план и подтвердил создание задач
- **Вход:** План реализации (передаётся в prompt)
- **Выход:** Задачи в br, организованные по фазам (epics) с зависимостями
- **Инструменты:** Все (All tools)
- **Не модифицирует** исходный код — только работает с br

Промпт агента определён в `.Codex/agents/create-tasks.md`.

### run-phase

**Тип:** `subagent_type: "run-phase"`

Агент выполнения фаз. Берёт все задачи указанной фазы из bd и последовательно выполняет их: пишет код, проверяет, закрывает задачи.

- **Триггер:** Пользователь вызывает `/run-phase N`
- **Вход:** Номер фазы, Epic ID или имя фазы
- **Выход:** Реализованный код + закрытые задачи + отчёт о завершении
- **Инструменты:** Все (All tools)
- **Проверяет** завершение предыдущих фаз перед началом работы
- **Обновляет** `architecture.md` при создании/удалении файлов

Промпт агента определён в `.Codex/agents/run-phase.md`.

### time-tracker

**Тип:** `subagent_type: "time-tracker"`

Агент трекинга времени. Фиксирует время выполнения задач и отправляет webhook-отчёты в Kimai Desktop через локальный сервер.

- **Триггер:** Завершение задачи или фазы, пользователь просит записать время
- **Вход:** Описание задачи, данные о времени (начало/конец)
- **Выход:** Webhook-событие в Kimai Desktop с realDuration и estimatedHumanDuration
- **Инструменты:** Все (All tools)
- **permissionMode:** dontAsk
- **Не модифицирует** исходный код — только отправляет curl-запросы на webhook-сервер

Промпт агента определён в `.Codex/agents/time-tracker.md

<!-- arhit:start -->

# Arhit — Инструмент архитектуры и документации

В этом проекте используется `arhit` — CLI для работы с архитектурой и документацией кода.
Все данные хранятся в `.arhit/` (JSON + Markdown), добавлены в git.

## Как использовать

### Перед началом работы с кодом
```bash
arhit context       # Получить полный контекст проекта (рекомендуется в начале сессии)
arhit arch build    # Построить/обновить граф архитектуры
arhit analyze       # Проанализировать зависимости
```

### Исследование кодовой базы
```bash
arhit search <запрос>              # Fuzzy-поиск по коду И докам (терпит опечатки) — начни отсюда
arhit explain <элемент>            # Всё об элементе разом: путь:строка, сигнатура, вызовы, зависимые, доки
arhit arch show                    # Показать архитектуру (JSON для агента)
arhit arch show --format tree      # Дерево архитектуры
arhit deps <элемент>               # Что зависит от элемента
arhit calls <элемент>              # Что элемент вызывает
arhit map --format mermaid         # Карта взаимодействий
```

### Документирование изменений
```bash
arhit doc add <элемент> --content "Описание"   # Задокументировать элемент
arhit doc show <элемент>                        # Прочитать документацию
arhit doc list                                  # Все задокументированные элементы
arhit doc search <запрос>                       # Fuzzy-поиск по докам (element, content, path, aliases)
arhit doc create <имя> --content "..."          # Свободная страница
arhit doc alias <элемент> <алиас>              # Добавить поисковый алиас к существующей записи
arhit doc coverage                              # Какие публичные элементы ещё не задокументированы
arhit doc stale                                 # Доки, устаревшие или указывающие на удалённый код
arhit arch build --seed-docs                    # Засеять доки из JSDoc/docstring исходников
```

## Правила для агента

### Исследование кодовой базы — ТОЛЬКО через arhit
- **НЕ используй Explore-агентов и массовый Grep/Glob для исследования структуры проекта.** Вместо этого используй команды `arhit`:
  - Найти что-либо по теме/имени → `arhit search <запрос>` (fuzzy, по коду и докам — первый шаг разведки)
  - Всё об элементе одним вызовом → `arhit explain <элемент>` (путь:строка, сигнатура, вызовы, зависимые, доки)
  - Структура и архитектура → `arhit arch show` / `arhit arch show --format tree`
  - Зависимости элемента → `arhit deps <элемент>`
  - Что вызывает элемент → `arhit calls <элемент>`
  - Карта взаимодействий → `arhit map --format mermaid`
  - Поиск по документации → `arhit doc search <запрос>`
  - Контекст проекта → `arhit context`
- Explore-агент и subagent_type=Explore допустимы **только** если arhit не содержит нужной информации (например, поиск конкретной строки в коде). В этом случае сначала проверь `arhit search`, и только потом переходи к прямому поиску.

### Документирование — ОБЯЗАТЕЛЬНО
- **Документируй ВСЮ разработку.** Каждое изменение в коде должно быть отражено в документации.
- При добавлении нового файла/функции/класса — сразу добавь документацию через `arhit doc add`
- При изменении существующего кода — обнови документацию: `arhit doc add <элемент> --content "новое описание"`
- При крупных изменениях (рефакторинг, новая фича, архитектурное решение) — создай страницу: `arhit doc create <имя> --content "описание изменений"`
- **Документация должна быть ИСЧЕРПЫВАЮЩЕЙ**, а не одно предложение. Для каждого элемента описывай:
  - Что делает элемент и зачем он нужен
  - Ключевые функции/методы и их параметры
  - Зависимости и связи с другими элементами
  - Примеры использования (если применимо)
  - Бизнес-логику и ограничения

### Обновление архитектуры
- После ЛЮБОГО изменения кода запусти `arhit arch build && arhit analyze` чтобы обновить данные
- Перед крупными изменениями проверь зависимости через `arhit deps` и `arhit calls`

### Алиасы документации
Поиск (`arhit search` / `arhit doc search`) теперь fuzzy и терпит опечатки и формы слов,
поэтому алиасы нужны реже. Но если запрос всё же ничего не нашёл — после выполнения задачи
закрепи его алиасом, чтобы будущие поиски по той же формулировке точно срабатывали:
```bash
# Привязать запрос к существующей записи
arhit doc alias <существующий-элемент> "<запрос>"

# Или создать новую запись и добавить алиас
arhit doc add <элемент> --content "..."
arhit doc alias <элемент> "<запрос>"
```

### Прочие правила
- Не редактируй файлы в `.arhit/` напрямую — используй команды CLI
- По умолчанию вывод в JSON (для агента), флаг `-H` для человеко-читаемого формата

<!-- arhit:end -->

## Запрет на /loop, ScheduleWakeup, CronCreate

- **НИКОГДА не используй `/loop`** (skill `loop`) — ни в fixed-interval, ни в dynamic режиме.
- **НИКОГДА не вызывай `ScheduleWakeup`** для self-paced продолжения задач.
- **НИКОГДА не вызывай `CronCreate`** для рекуррентных задач в этом проекте.
- Если пользователь явно запросил `/loop` или просит «продолжай каждые N минут» — откажись и предложи запустить нужную задачу один раз сейчас, либо предложи `/schedule` (cloud) если это осознанный выбор пользователя.
- Если задача требует ожидания внешнего события (CI, download, build) — выполняй её в фоне (`run_in_background: true` в Bash) и докладывай по завершению через `<task-notification>`, **без** ScheduleWakeup-периодических проверок.

<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

### Essential Commands

```bash
# View ready issues (open, unblocked, not deferred)
br ready              # or: bd ready

# List and search
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br search "keyword"   # Full-text search

# Create and update
br create --title="..." --description="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once

# Sync with git
br sync --flush-only  # Export DB to JSONL
br sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Always run `br sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only open, unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads changes to JSONL
git commit -m "..."     # Commit everything
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always sync before ending session

<!-- end-br-agent-instructions -->
