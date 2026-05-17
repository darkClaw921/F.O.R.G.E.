# tmux-web/static/style.css

CSS layout tmux-web. Содержит стили для основного layout (sidebar/top-bar/panels), tab-кнопок, terminal-обёртки, tasks-board, themes, modals, remote-panel и TUI-вкладок (lazygit/lazydocker/telescope) + mobile-адаптацию (Phase A/B/C).

## Layout

Flex-based: app=row (sidebar | main). main=column (top-bar | panels). Каждая panel (#terminal/#tasks/#git/#docker/#telescope) — flex column с заполнением высоты.

## TUI-вкладки (Phase 2, forge-chjx)

### Общие .tui-* классы

Введены в Phase 2 для трёх вкладок (#git, #docker, #telescope). Заменяют дублирование старых .git-*.

- .tui-term — контейнер xterm.js: flex:1, position:relative, фон #000, padding 6px. xterm-дети (.xterm, .xterm-viewport, .xterm-screen) форсятся 100%×100%. [hidden] → display:none !important.
- .tui-placeholder — центрированный текст когда нет активного проекта (var(--fg-dim)). [hidden] → display:none !important.
- .tui-error — красный banner (order:-1 в flex column — выше term), фон var(--danger), border, padding, flex-row gap 8.
- .tui-error-text — gap-filling текстовый узел (flex:1).
- .tui-error-retry / .tui-error-close — кнопки. .tui-error-close — × dismiss, .tui-error-retry — синяя ('Retry').
- .tui-install-help — раскрывающийся блок с install-командами. Содержит .tui-install-title (header), ul.tui-install-list, .tui-install-link.
- .tui-install-list li — flex-row: .os-label + .os-cmd (code) + .os-copy (button). .os-label.detected получает ::after маркер (например, ★) для подсветки текущей ОС.
- .os-copy / .os-copy.copied — состояние кнопки Copy.

### Контейнеры

- #docker, #telescope — flex column (как #git), заполняются xterm-инстансами через .tui-term. [hidden] → display:none, иначе display:flex.

### #git стили

Phase 4 оригинальные .git-term/.git-placeholder/.git-error сохранены для backward compat.

## Theming

CSS-переменные (var(--bg-base), --fg, --fg-dim, --accent, --danger, --tab-active, ...).

## Mobile-адаптация (Phase A/B/C)

Три mobile-секции в конце файла (~ строки 2805-3015):
1. `@media (max-width: 768px)` — основная mobile-сетка.
2. `@media (max-width: 480px)` — узкие телефоны (мельче шрифты, компактнее padding).
3. `@media (max-width: 768px) and (prefers-reduced-motion: reduce)` — отключение transition на off-canvas sidebar.

### Off-canvas sidebar mechanism

На desktop sidebar — `position:relative; width:240px`. На mobile (max-width:768px):
- `aside.sidebar` → `position:fixed; left:0; top:0; bottom:0; transform:translateX(-100%); transition: transform 0.25s ease`.
- `body.sidebar-open aside.sidebar` → `transform:translateX(0)` (выезжает справа налево).
- `#sidebar-overlay` (sibling после `</aside>`) — fullscreen полупрозрачный overlay (`position:fixed; inset:0; background:rgba(0,0,0,0.5); z-index: just below sidebar`). По умолчанию `hidden`. При `body.sidebar-open` → `display:block`. Клик по нему → закрытие сайдбара (handler в app.js).
- Класс `body.sidebar-open` ставится JS из `setMobileSidebarOpen(true)` в app.js.

### .quick-cmd-bar (внутри #terminal)

Контейнер для top-N команд + spec-keys + Edit. На mobile фиксируется внизу `#terminal`:
- `position:absolute; left:0; right:0; bottom:0`.
- `padding-bottom: env(safe-area-inset-bottom)` — учёт home-bar iOS.
- `backdrop-filter: blur(6px)` + полупрозрачный background для эффекта стеклянной панели поверх xterm.
- `display:flex; flex-direction:row; gap:4px; overflow-x:auto; -webkit-overflow-scrolling:touch` — горизонтальный scroll если кнопок много.
- `#terminal` и `.tui-term` получают `padding-bottom: 56px` чтобы bar не закрывал последние строки терминала.

### .quick-cmd-key / .quick-cmd-cmd / .quick-cmd-edit

Внутренние кнопки bar. Минимальный touch-target — 44×44px (рекомендация Apple HIG). Стилизация:
- `min-width:44px; min-height:44px; padding:8px 12px`.
- Border-radius 6px, фон var(--bg-elevated), цвет var(--fg).
- `.quick-cmd-cmd` — text wrap отключен, `white-space:nowrap`.
- `.quick-cmd-key` — моноширинный шрифт (для spec-keys типа Esc, ^C).
- Active-состояние (`:active`) — затемнение фона для тактильного фидбэка.

### .tui-quick-bar (внутри #git / #docker / #telescope)

Аналогично `.quick-cmd-bar`, но без `.quick-cmd-edit` (TUI-набор фиксирован). DOM-id: `#git-quick-bar`, `#docker-quick-bar`, `#telescope-quick-bar`. На desktop скрыты через `hidden` (управляется JS). На mobile показываются только когда соответствующая TUI-панель не hidden.

`.tui-quick-key` — touch-target 44×44, отображает символ (`q`, `Esc`, `↑` и т.п.).

### Kanban (Tasks) — горизонтальный scroll-snap

На mobile `#tasks-board` теряет grid-сетку и становится горизонтальной лентой:
- `display:flex; flex-direction:row; overflow-x:auto; scroll-snap-type: x mandatory`.
- Каждая колонка статуса (`.kanban-column`) — `flex:0 0 85vw; scroll-snap-align:start`.
- Это даёт UX как в Trello/Linear mobile: палец листает колонки.

### Full-screen модалки

Все модалки (settings, edit-task и т.п.) на mobile занимают полный экран:
- `.modal-content` → `width:100vw; height:100vh; max-width:none; border-radius:0`.
- Заголовок и кнопка закрытия — `position:sticky; top:0` чтобы оставались видимыми при скролле.

### Touch-target минимумы

Все интерактивные элементы — `min-height:44px` (хоткеи top-bar, sidebar items, tab-кнопки).

### prefers-reduced-motion

Третья media-секция отключает `transition: transform` на sidebar чтобы пользователи с настройкой reduce-motion не страдали от анимации.

## Зависимости

- xterm.js (window.Terminal) — рендеринг.
- Theme manifest из ThemeStore.

## Связанные файлы

- [tmux-web/static/index.html](tmux-web/static/index.html) — DOM-структура (содержит классы и id, на которые ссылаются стили).
- [tmux-web/static/app.js](tmux-web/static/app.js) — JS-логика, ставит `body.sidebar-open`, управляет `hidden` на quick-bar-ах.
- [tmux-web/static/quick-cmd.js](tmux-web/static/quick-cmd.js) — рендеринг содержимого `.quick-cmd-bar` / `.tui-quick-bar`.
