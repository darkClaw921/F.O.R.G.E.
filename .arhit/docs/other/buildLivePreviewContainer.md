# buildLivePreviewContainer

Phase 5: фабрика live-preview блока для редактора тем.

Сигнатура: buildLivePreviewContainer() → { el: HTMLElement, update(draft: {ui, term}): void }

Возвращает .theme-editor-preview контейнер с двумя секциями:

1. .theme-preview-ui — мини-приложение:
   - .theme-preview-sidebar (бэкграунд var(--bg-elev)): заголовок 'Sessions' + список (main/logs/editor) с активным элементом (var(--accent) бэкграунд).
   - .theme-preview-main (фон var(--bg)): .theme-preview-text (var(--fg)), .theme-preview-text-dim (var(--fg-dim)), .theme-preview-tags (3 пилюли P0/P1/P2 на var(--p0/p1/p2)), .theme-preview-buttons (accent/warn/danger).

2. .theme-preview-term — мини-терминал:
   - 4 строки: 'ls --color' prompt, 8 base ANSI цветных span'ов, 8 bright ANSI, cursor + selection sample.

update(draft):
- 11 ui-цветов выставляются как scoped CSS-переменные (--bg, --bg-elev, --fg, --fg-dim, --border, --accent, --warn, --danger, --p0, --p1, --p2) на КОРНЕВОМ .theme-editor-preview через el.style.setProperty. Внутри потомки используют var(...) — это даёт scoped-применение БЕЗ влияния на :root приложения.
- 20 term-цветов — inline стили: term.style.background/color/border + по-span'но color = ANSI цвет + cursor.style.background/color (инверсия) + selection.style.background/color.

Зависимости: вызывается openThemeEditor; CSS-классы .theme-preview-* в style.css.
