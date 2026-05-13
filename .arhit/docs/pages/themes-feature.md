# Themes feature (forge-wk7)

## Обзор

Поддержка переключаемых тем (UI + терминал) для tmux-web — аналог тем в Termius. Одна Theme описывает одновременно палитру UI (фон, текст, акценты, рамки, priority-цвета) и палитру терминала (16 ANSI + foreground/background/cursor/selection). Темы делятся на встроенные пресеты (9 шт) и пользовательские кастомные. Хранение глобальное (один набор активная-тема + custom-темы на пользователя, не per-project). UI — отдельная вкладка 'Themes' в settings modal с превью-карточками, редактором палитры и live preview.

Ключевая идея: переключение темы происходит мгновенно без перезагрузки страницы и без пересоздания xterm Terminal. CSS-переменные :root обновляются за один вызов, xterm.options.theme меняется через горячий API xterm.js 5.x.

## Архитектура

Backend (Rust / Axum):
- tmux-web/src/themes.rs — модель Theme/UiColors/TermColors, ThemesState, load/save/built_in_presets/find_preset.
- tmux-web/src/main.rs — AppState содержит themes: Arc<RwLock<ThemesState>> + themes_dir: PathBuf; 5 REST endpoints.

Storage:
- ~/.config/forge/themes.json — { active: String, custom: Vec<Theme> }. Пресеты в файл НЕ записываются — компилируются в бинарь. Атомарная запись через write-tmp + rename. Fallback на default при отсутствии/повреждении.

REST API:
- GET /api/themes — { presets, custom, active }.
- GET /api/themes/active — полный объект активной темы.
- PATCH /api/themes/active — { id }, переключить активную (валидация против presets+custom).
- POST /api/themes/custom — создать (uuid v4 если id пустой; 409 при конфликте).
- PUT /api/themes/custom/:id — заменить (404 если нет; 409 если id пресета).
- DELETE /api/themes/custom/:id — удалить (409 если активна; 204 при успехе).

Frontend (vanilla JS + CSS):
- tmux-web/static/style.css — :root с 11 themable + ~58 secondary CSS-переменными; стили theme-card, theme-card-grid, modal-tabs, theme-editor-modal.
- tmux-web/static/app.js — applyTheme/mapTermTheme/switchTheme/loadActiveThemeOrNull (runtime); loadThemesIntoPanel/renderThemesPanel/buildThemeCard (UI-вкладка); openThemeEditor (редактор с пикерами и live preview).

## Поток данных

### Bootstrap (загрузка страницы)
1. bootstrap() (async) → loadActiveThemeOrNull() → GET /api/themes/active.
2. applyTheme(theme): 11 CSS-переменных через document.documentElement.style.setProperty + state.activeTheme = theme.
3. Возврат mapTermTheme(theme.term) → передаётся в initTerminal(termTheme) → new Terminal({ theme: ... }) — xterm создаётся уже с правильным background.
4. Если GET упал (offline) → null → initTerminal использует hard-coded fallback.

### Switch theme (клик по карточке)
1. switchTheme(id) → PATCH /api/themes/active с { id }.
2. GET /api/themes/active → applyTheme(theme).
3. CSS обновляется через :root setProperty (11 переменных).
4. xterm: state.term.options.theme = mapTermTheme(theme.term) — горячая смена без пересоздания.
5. renderThemesPanel перерисовывается, чтобы обновить .active class на карточках.

### Custom CRUD
1. New: openThemeEditor(null) → редактор с baseline draft → Save → POST → loadThemesIntoPanel.
2. Edit: openThemeEditor(theme) → пикеры заполнены текущими значениями → live preview → Save → PUT → loadThemesIntoPanel.
3. Delete: confirm → DELETE → loadThemesIntoPanel (показ ошибки 409 если активна).

## 9 встроенных пресетов

Возвращаются built_in_presets() в фиксированном порядке:
1. default — Default (baseline tmux-web, #0e1116 / #d8dee9, accent #2a7fff). 1:1 соответствие :root в style.css.
2. dracula — Dracula (#282a36 / #f8f8f2, accent #bd93f9).
3. solarized-dark — Solarized Dark (#002b36 / #839496, accent #268bd2).
4. solarized-light — Solarized Light (#fdf6e3 / #657b83, инверсия).
5. monokai — Monokai (#272822 / #f8f8f2, accent #a6e22e).
6. nord — Nord (#2e3440 / #d8dee9, accent #88c0d0).
7. gruvbox-dark — Gruvbox Dark (#282828 / #ebdbb2, accent #fabd2f).
8. one-dark — One Dark (#282c34 / #abb2bf, accent #61afef).
9. tokyo-night — Tokyo Night (#1a1b26 / #a9b1d6, accent #7aa2f7).

Каждый пресет полностью заполняет UiColors (11) и TermColors (20). Используются официальные палитры соответствующих проектов. Default — fallback при повреждённом state.active.

## Custom themes — CRUD и ограничения

- id-namespace общий с пресетами: создание custom с id 'dracula' → 409.
- id генерируется uuid v4 если не передан (поле trim'ится перед проверкой).
- PUT перезаписывает id из URL (тело id игнорируется) — фронт не должен дублировать.
- Активную тему нельзя удалить: DELETE с id == state.active → 409.
- Пресеты иммутабельны: PUT/POST с id пресета → 409.
- Все мутации сохраняют themes.json атомарно (write-tmp + rename). При IO-ошибке save → 500, изменение в памяти не откатывается.
- При битом state.active (id не найден ни в пресетах, ни в custom) — GET /api/themes/active отдаёт 'default' пресет с tracing::warn!.

## UI: Themes-вкладка в settings modal

Settings modal расширен tab-bar с двумя вкладками (Phase wk7.4):
- Notifications — исторический контент (без изменений).
- Themes — lazy-load при первом клике через loadThemesIntoPanel.

Themes-панель содержит две секции с .theme-card-grid:
1. Presets — 9 карточек встроенных тем. Click → switchTheme.
2. Custom themes — карточки пользовательских тем + кнопка '+ New' в header. Каждая карточка имеет .theme-card-tools (Edit / Delete).

Карточка темы (.theme-card):
- .theme-card-name — имя.
- .theme-card-preview — горизонтальная полоса из 10 swatch'ей: background, foreground (шире, как 'лицо'), 6 ANSI (red/green/yellow/blue/magenta/cyan), accent, cursor.
- .theme-card-badge 'ACTIVE' — индикатор для активной темы.
- :hover / .active — border-color = accent.

## Редактор кастомной темы (Phase wk7.5)

openThemeEditor(themeOrNull) — модал с пикерами и live preview.

Структура:
1. Header: 'Edit theme' / 'New theme' + close-btn.
2. Body (scrollable, max-height 88vh):
   - Meta: name input + 'Duplicate from preset' select (копирует ui+term из выбранного пресета в draft — удобно стартовать с похожей палитры).
   - UI section: 11 color pickers (input type=color) для bg, bgElev, fg, fgDim, border, accent, warn, danger, p0, p1, p2.
   - Term base section: 4 пикера для foreground, background, cursor, selection.
   - ANSI section: 8 base + 8 bright цветов в компактных строках (.theme-editor-color-row-compact).
   - Live preview: xterm-подобный preview-block, перерисовывается на каждый change через локальную renderPreview(draft) — показывает draft без сохранения на сервер.
3. Footer: Cancel / Save.

Save: POST для новой темы / PUT для существующей. После успеха — закрыть модал + loadThemesIntoPanel (полная перерисовка). Валидация на бекенде (409 при конфликте id с пресетом).

## Файлы фичи

Новые файлы:
- [tmux-web/src/themes.rs](tmux-web/src/themes.rs) — модель + хранилище + 9 пресетов + тесты.

Изменённые файлы:
- [tmux-web/src/main.rs](tmux-web/src/main.rs) — AppState (themes, themes_dir), 5 REST handlers, mod themes, импорт.
- [tmux-web/static/app.js](tmux-web/static/app.js) — applyTheme/mapTermTheme/switchTheme/loadActiveThemeOrNull в bootstrap; settings modal с tab-bar; loadThemesIntoPanel/renderThemesPanel/buildThemeCard; openThemeEditor с пикерами и live preview.
- [tmux-web/static/style.css](tmux-web/static/style.css) — рефакторинг на :root CSS-переменные; стили modal-tabs, theme-card, theme-card-grid, theme-editor-modal.

Новый storage-файл (runtime): ~/.config/forge/themes.json.

## Декомпозиция (для исторической справки)

Эпик forge-wk7 разбит на 6 фаз:
- forge-wk7.1 Phase 1: Backend — модель тем и REST API.
- forge-wk7.2 Phase 2: CSS refactor на CSS-переменные.
- forge-wk7.3 Phase 3: Frontend — applyTheme + bootstrap.
- forge-wk7.4 Phase 4: UI — вкладка Themes в settings modal.
- forge-wk7.5 Phase 5: Редактор кастомной темы.
- forge-wk7.6 Phase 6: Документация (arhit) — текущая.