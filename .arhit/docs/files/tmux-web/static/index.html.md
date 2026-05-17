# tmux-web/static/index.html

HTML-шаблон tmux-web frontend. Содержит layout: левый sidebar (sessions/projects), top-bar с tab-кнопками, и панели-вкладки terminal/tasks/git/docker/telescope.

## Tab-кнопки (top-bar)

- #tab-terminal — Terminal (tmux attach).
- #tab-tasks — Tasks (beads-board).
- #tab-git — Git (lazygit TUI).
- #tab-docker — Docker (lazydocker TUI, Phase 2).
- #tab-telescope — Find (television fuzzy finder, Phase 2). Подпись 'Find' — semantically более понятна, чем 'Telescope'.

## Панели вкладок

Phase 4: git-pane — xterm.js терминал к /ws/lazygit.
Phase 2 (forge-chjx): добавлены панели #docker и #telescope — зеркальные #git, но с другими ID-суффиксами.

### Общая структура TUI-панелей (#git, #docker, #telescope)
- {prefix}-placeholder — текст-заглушка ('Select a project to open <tui>'), виден когда нет активного проекта. Phase 2 заменён на класс .tui-placeholder.
- {prefix}-error — banner-плашка с .tui-error-text, .tui-error-retry и .tui-error-close (×). hidden по умолчанию.
- {prefix}-install-help — раскрывающийся блок с install-командами (виден при binary-not-found). Содержит .tui-install-title + ul.tui-install-list + a.tui-install-link.
- {prefix}-term — контейнер xterm.js Terminal (.tui-term). Изолированная инстанция на каждую вкладку.

### DOM ids

git-pane: #git-placeholder, #git-error, #git-error-text, #git-error-retry, #git-error-close, #git-install-help, #git-install-list, #git-term, #git-legacy.

docker-pane (Phase 2): #docker-placeholder, #docker-error, #docker-error-text, #docker-error-retry, #docker-error-close, #docker-install-help, #docker-install-list, #docker-term.

telescope-pane (Phase 2): #telescope-placeholder, #telescope-error, #telescope-error-text, #telescope-error-retry, #telescope-error-close, #telescope-install-help, #telescope-install-list, #telescope-term.

### #git-legacy

Обёртка над старой git-разметкой (toolbar/files/commit/graph), оставлена hidden до Phase 5 cleanup. Нужна чтобы legacy DOM-refs не были null при загрузке app.js.

## Mobile-расширения (Phase A/B/C)

### #sidebar-overlay

`<div id="sidebar-overlay" hidden>` добавлен сразу после `</aside>` (закрытия сайдбара). Это полупрозрачный fullscreen overlay, видимый только при открытом мобильном сайдбаре (`body.sidebar-open` + mobile viewport). Цель — затемнить фон контента и обеспечить click-to-close для off-canvas сайдбара. CSS-стили — в `style.css` (`#sidebar-overlay`). JS-handler (click + Esc) — в `app.js` (см. `setMobileSidebarOpen`).

### #quick-cmd-bar (внутри #terminal)

`<div id="quick-cmd-bar">` добавлен внутри panel `#terminal`, после `<div id="terminal-term">`. Структура:
- `<div class="quick-cmd-keys">` — slot для spec-клавиш (Esc, Tab, ^C, стрелки) — рендерится JS-модулем `quick-cmd.js`.
- `<div class="quick-cmd-cmds">` — slot для top-N команд (динамически из localStorage `forge.quickCmd.*`).
- `<button class="quick-cmd-edit">` — кнопка открытия редактора команд.

Видимость: на desktop bar — обычная горизонтальная панель внизу `#terminal`. На mobile (max-width:768px) — `position:absolute; bottom:0` с `safe-area-inset-bottom`. Стили см. в `style.css` (`.quick-cmd-bar`, `.quick-cmd-key`, `.quick-cmd-cmd`, `.quick-cmd-edit`).

### .tui-quick-bar (внутри #git / #docker / #telescope)

В каждой из трёх TUI-панелей добавлен `<div class="tui-quick-bar" id="{git|docker|telescope}-quick-bar">`. Это аналог `#quick-cmd-bar`, но с fixed-набором TUI-клавиш (`q, Esc, ?, :, /, h, j, k, l, Enter, ^C, Tab, ↑↓←→`). Рендеринг и обработка кликов — в `quick-cmd.js` (`refreshTuiBars`). Видимость управляется атрибутом `hidden` через MutationObserver + matchMedia (mobile-only).

## Подключаемые скрипты

xterm.js + addon-fit + addon-web-links (CDN/embedded). 

- `<script src="/app.js">` — главный IIFE-modul (state, WS, TUI-tabs, hotkeys).
- `<script src="/quick-cmd.js">` — IIFE-modul quick-cmd-bar (подключён после `app.js`, так как зависит от `window.ForgeApp.sendToActivePty`). Phase B/C.

## Связи

- Tab-кнопки → switchTab() в app.js.
- {prefix}-term → state.gitTerm/dockerTerm/telescopeTerm через initTuiTabs() (создаёт createTuiTab инстансы с DOM-refs).
- {prefix}-install-help → install-help блок, показывается createTuiTab при binary-not-found.
- #sidebar-overlay → click/Esc handlers в app.js (setMobileSidebarOpen / Esc-handler).
- #quick-cmd-bar / #git-quick-bar / #docker-quick-bar / #telescope-quick-bar → JS-рендеринг и обработка в quick-cmd.js (`window.QuickCmd.refresh / refreshTuiBars`).
- Гамбургер-кнопка сайдбара (на mobile) → `toggleSidebar()` в app.js (mobile-ветка через `isMobileViewport()`).
