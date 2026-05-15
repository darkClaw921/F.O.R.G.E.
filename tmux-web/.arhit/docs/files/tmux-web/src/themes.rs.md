# tmux-web/src/themes.rs

Theme model + storage для tmux-web (Phase wk7).

## Назначение

Описывает цветовую тему UI + терминала. Состоит из:
- Theme { id, name, ui: UiColors, term: TermColors }
- UiColors — 11 CSS-переменных (bg, bgElev, fg, fgDim, border, accent, warn, danger, p0, p1, p2)
- TermColors — 20 цветов терминала (foreground, background, cursor, selection + 16 ANSI: black..white + brightBlack..brightWhite)

Все цвета — String в формате #RRGGBB lowercase.

## Сериализация

serde с rename_all=camelCase. Опциональных полей нет — каждая тема обязана задать все 31 цвет.

## Файловое хранилище

ThemesState { active: String, custom: Vec<Theme> } сериализуется в themes.json (рядом с projects.json в ~/.config/forge/). Атомарное сохранение через tmp+rename. Пресеты не сохраняются — компилируются в бинарь.

load(data_dir) — fallback на ThemesState::default() при отсутствии файла или ошибке парсинга. Не паникует.

## Built-in presets (13 тем, обновлено forge-j0pd)

built_in_presets() возвращает в фиксированном порядке:
1. Default — baseline tmux-web (#0e1116 / #d8dee9)
2. Dracula — dracula/dracula-theme
3. Solarized Dark — Ethan Schoonover
4. Solarized Light — same palette inverted
5. Monokai — Wimer Hazenberg
6. Nord — arcticicestudio/nord
7. Gruvbox Dark — morhetz/gruvbox medium contrast
8. One Dark — atom/one-dark-syntax
9. Tokyo Night — enkia/tokyo-night storm/dark
10. Catppuccin Latte — light flavor (catppuccin/catppuccin)
11. Catppuccin Frappé — medium-dark flavor
12. Catppuccin Macchiato — darker flavor
13. Catppuccin Mocha — darkest flavor (default Catppuccin)

find_preset(id) — Option<Theme> поиск по id.

## REST API (см. main.rs)

- GET /api/themes → { presets, custom, active }
- GET /api/themes/active → полный Theme
- PATCH /api/themes/active → переключить активную
- POST /api/themes/custom → добавить пользовательскую
- PUT /api/themes/custom/:id → заменить пользовательскую
- DELETE /api/themes/custom/:id → удалить (запрет если активна)

## Tests

6 unit-тестов: presets_count_and_unique_ids (=13, уникальные id), camel_case_serde, load_missing_returns_default, save_then_load_roundtrip, corrupt_file_falls_back_to_default, find_preset_works (включая 4 catppuccin id).
