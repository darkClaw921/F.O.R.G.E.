# tmux-web/src/themes.rs

Модуль модели тем и хранилища для tmux-web (themes.rs). Глобальные темы — без привязки к project (была и остаётся одна activeId на пользователя).

## Назначение

Описывает цветовую тему UI + терминала + предоставляет атомарное файловое хранилище состояния тем (themes.json в data_dir, обычно ~/.config/forge/). Используется бэкендом для отдачи REST /api/themes* и фронтендом для применения CSS-переменных + xterm ITheme. Темы делятся на встроенные пресеты (компилируются в бинарь) и пользовательские (хранятся в themes.json).

## Структуры

### Theme
Полная цветовая тема. Поля:
- id: String — kebab-case slug, уникален во всём пространстве (presets + custom).
- name: String — отображаемое имя.
- ui: UiColors — палитра CSS-переменных.
- term: TermColors — палитра терминала.
Сериализация: serde camelCase. Все цвета — String '#RRGGBB'.

### UiColors (11 hex-значений)
bg, bgElev, fg, fgDim, border, accent, warn, danger, p0, p1, p2.

### TermColors (20 hex-значений → xterm.js ITheme)
foreground/background/cursor/selection + ANSI 8 базовых + ANSI 8 ярких.

### ThemesState (envelope для themes.json)
- active: String — id активной темы. Может ссылаться на preset или custom.
- custom: Vec<Theme> — пользовательские темы.

## Функции

- built_in_presets() — 9 пресетов: default, dracula, solarized-dark, solarized-light, monokai, nord, gruvbox-dark, one-dark, tokyo-night.
- find_preset(id) — поиск пресета.
- themes_file_path(data_dir) — data_dir.join('themes.json').
- load(data_dir) — fallback на default при любых ошибках.
- save(data_dir, state) — atomic save (tempfile + rename).

## REST API

GET /api/themes, GET /api/themes/active, PATCH /api/themes/active, POST /api/themes/custom, PUT /api/themes/custom/:id, DELETE /api/themes/custom/:id.

## Бизнес-логика и ограничения

- Пресеты иммутабельны (PUT/POST с id пресета → 409).
- Активную тему нельзя удалить.
- Persist гарантия: каждая мутация сначала меняет in-memory state, затем themes::save.

## Файл

tmux-web/src/themes.rs. Один на пользователя глобально (никогда не было per-project).
