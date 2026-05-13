# static/style.css#git-styles

Стили вкладки Git добавлены в конец static/style.css (Phase 3, секция '===== Git tab (Phase 3) ====='). Размер ~2.9KB.

Используют существующие CSS-переменные темы — все 11 themable-переменных + производные (--bg, --bg-elev, --bg-toolbar, --bg-input, --bg-pill-hover, --bg-deep, --border, --border-soft, --border-input, --accent, --fg, --fg-dim, --warn, --danger, --success). При смене темы через applyTheme() Git tab перекрашивается автоматически.

Ключевые селекторы:
- #git: flex column, скрытие через [hidden] / показ через :not([hidden]).
- #git-toolbar: горизонтальный flex, padding 8px 12px, border-bottom, background --bg-toolbar.
- #git-branch: bold + --accent (имя ветки выделено).
- #git-ahead-behind: --fg-dim, font-size 12px (приглушённый счётчик).
- #git-body: CSS grid 1fr/1fr/1.4fr (graph-pane шире — для канваса), gap 1px на --border-soft даёт тонкие разделители между секциями.
- #git-files-pane / #git-commit-pane / #git-graph-pane: общий background --bg, overflow auto, padding 12px.
- h3 внутри секций: uppercase 12px --fg-dim (subdued заголовки).
- file-row labels (#git-staged-list label, #git-unstaged-list label): flex gap 8px, monospace 13px, hover --bg-pill-hover.
- .git-badge (модификаторы): width 14px, центрированный 11px bold. Цветовые классы:
  - modified=--warn/--bg
  - added=--success/--bg
  - deleted=--danger/#fff
  - untracked=--fg-dim/--bg
  - renamed=--accent/#fff
  - conflict=--danger/#fff
  - empty=transparent/--fg-dim
- #git-commit-msg: full-width textarea с --bg-input, --border-input, monospace.
- #git-commit-btn: margin-top 8px (наследует .primary стили из tasks-toolbar).
- #git-commit-error: --danger, white-space pre-wrap (многострочные git-stderr), monospace.
- #git-graph-canvas: display block, background --bg-deep (затемнённый фон под граф).

Phase 3 — tw-nmy. CSS validated (269/269 braces balanced).
