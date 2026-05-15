# tmux-web/static/style.css

CSS layout tmux-web. Содержит стили для основного layout (sidebar/top-bar/panels), tab-кнопок, terminal-обёртки, tasks-board, themes, modals, remote-panel и TUI-вкладок (lazygit/lazydocker/telescope).

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
- .os-copy / .os-copy.copied — состояние кнопки Copy (анимация после успешного copy-to-clipboard).

### Контейнеры

- #docker, #telescope — flex column (как #git), заполняются xterm-инстансами через .tui-term. [hidden] → display:none, иначе display:flex.

### #git стили

Phase 4 оригинальные .git-term/.git-placeholder/.git-error сохранены для backward compat, но новый код использует .tui-*. По мере миграции legacy-классы можно убрать.

## Theming

CSS-переменные (var(--bg-base), --fg, --fg-dim, --accent, --danger, --tab-active, ...) — все TUI-стили совместимы с темами Phase 3.

## Зависимости

- xterm.js (window.Terminal) — рендеринг.
- Theme manifest из ThemeStore (загружается через JS, применяется через CSS-переменные на :root).
