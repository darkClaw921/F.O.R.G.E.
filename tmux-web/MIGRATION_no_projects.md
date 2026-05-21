# Migration: концепция Project удалена (cwd-only)

Эта инструкция применима к апгрейду F.O.R.G.E. с версий, где была концепция «Project», на версии после слияния `remove-projects-concept` (план: `/Users/igorgerasimov/.claude/plans/remove-projects-concept.md`).

## Что произошло

Концепция «Project» полностью удалена. Источник истины — `cwd` сессии (`session.path`). Группировка/фильтрация в UI идёт только по folder-headers в sidebar (никаких «активного проекта», `project_id`, `tmux_prefix`).

«Корень» для TODO/notifier теперь резолвится автоматически:

1. идём вверх по `cwd`, ищем первую папку с `.beads/`;
2. иначе ищем первую папку с `.git/`;
3. иначе fallback — сам `cwd`.

Реализация — `tmux-web/src/paths.rs::resolve_root(cwd)`.

## Что произойдёт с данными

### TODO (`~/.config/forge/todos.json`)

**Мигрируются автоматически** при первом запуске новой версии:

- На загрузке `TodoStore::load_with_projects(todos_path, projects_path)` определяет, нужна ли миграция: проверяет наличие `project_id` в JSON или невалидный `root_path` (не выглядит абсолютным путём).
- При миграции читается `~/.config/forge/projects.json` (один раз) и строится маппинг `project_id → project.path`.
- Каждая Todo-карточка переезжает в новый bucket `by_path[project.path]`, поле `Todo.project_id` заменяется на `Todo.root_path` (`#[serde(alias = "project_id")]` обеспечивает backward-compat для парсинга).
- После первой записи `todos.json` уже в новом формате — `projects.json` больше не читается.

**Fallback** (когда `projects.json` отсутствует или не содержит нужного `project_id`): `project_id` остаётся как `root_path` (deg-fallback, данные не теряются, в лог пишется `tracing::warn`). Группировка в этом случае будет по строке `project_id`, а не по абсолютному пути; чтобы починить — можно вручную отредактировать `~/.config/forge/todos.json` и заменить `root_path` на абсолютный путь корня.

### projects.json (`~/.config/forge/projects.json`)

**Остаётся на диске**, но **новой версией не читается** (кроме однократной миграции, описанной выше). Можно:

- оставить как есть — не мешает;
- удалить вручную — `rm ~/.config/forge/projects.json`;
- сохранить для возможного отката (см. ниже).

### Notifier config

Раньше настройки notify (`notify_template`, `notify_delay_minutes`, `notify_wait_previous`, `notify_session`) хранились в `Project` (per-project, в `projects.json`).

После апгрейда — **глобальный конфиг в `~/.config/forge/notifier.json`**, один на пользователя. Применяется ко всем `POST /api/todos/:id/promote`.

**Notifier-настройки автоматически НЕ мигрируются** — после первого запуска новой версии конфиг будет с дефолтами (пустой template ⇒ notify не отправляется). Чтобы восстановить старое поведение:

1. UI: `Settings → Notifications` — заполните template, delay, wait_previous, session.
2. Или REST: `PATCH /api/notifier-config` body `{ "template": "...", "delay_minutes": 0, "wait_previous": false, "session": "main" }`.

### Сессии (tmux)

`tmux_prefix` снят с фильтрации — все tmux-сессии всегда видны в sidebar. Группировка только по `folder_id` (cwd-derived). Если вы привыкли видеть сессии «только активного проекта» — этого больше нет, появится плоский список с папочной группировкой.

### Themes / user_settings

Без изменений — они и раньше были глобальные (один активный + custom-список на пользователя).

## Откат

Если что-то сломалось:

### Вариант 1: вернуться к предыдущему тегу

```bash
cd /path/to/F.O.R.G.E.
git fetch --tags
git checkout v0.1.23     # или ваш последний рабочий тег
cargo run -p devforge --release
```

`projects.json` остался на диске → старая версия его подхватит.

**Важно**: если новая версия успела мигрировать `todos.json`, при откате старая версия не сможет его прочитать (нет поля `project_id`). Решение — заранее сохранить копию: `cp ~/.config/forge/todos.json ~/.config/forge/todos.json.bak` ДО первого запуска новой версии.

### Вариант 2: revert через git

```bash
git revert <last-phase-5-commit>..<last-phase-1-commit>
cargo build -p devforge --release
```

Это вернёт код, но конфиги на диске останутся в новом формате. Аналогично варианту 1 — заранее сделайте backup `todos.json`.

## Резюме команд

```bash
# Backup ДО апгрейда (рекомендуется)
cp ~/.config/forge/todos.json ~/.config/forge/todos.json.bak
cp ~/.config/forge/projects.json ~/.config/forge/projects.json.bak

# Запуск новой версии (автомиграция TODO)
cargo run -p devforge --release

# После апгрейда — настроить notifier (если использовали)
curl -X PATCH http://127.0.0.1:7331/api/notifier-config \
  -H 'Content-Type: application/json' \
  -d '{"template": "Новая задача: {title}", "delay_minutes": 0, "wait_previous": false, "session": "main"}'

# Откат при проблемах
cp ~/.config/forge/todos.json.bak ~/.config/forge/todos.json
git checkout v0.1.23   # последний релиз ДО Phase 1
cargo run -p devforge --release
```

## См. также

- `arhit doc show remove-projects-concept` — обзорная страница с разбивкой по фазам.
- `arhit doc show tmux-web/src/paths.rs` — детали `resolve_root`.
- `arhit doc show notifier_config` — детали NotifierConfigStore.
- `arhit doc show tmux-web/src/todos.rs` — детали миграции TodoStore.
- Memory: `[[project_no_projects]]` в `~/.claude/projects/.../memory/`.
- План: `/Users/igorgerasimov/.claude/plans/remove-projects-concept.md`.
