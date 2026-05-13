# tmux-web/static/style.css

CSS layout tmux-web. Phase 4: добавлены стили для xterm-git-tab:
- .git-term — flex:1 контейнер xterm.js, position:relative, фон #000, padding 6px
- .git-placeholder — центрированный текст когда нет активного проекта (использует var(--fg-dim))
- .git-error — красный banner (order:-1 в flex column, перед term), с .git-error-text (gap-filling), .git-error-retry, .git-error-close. Hidden по умолчанию
- xterm-дети (.xterm, .xterm-viewport, .xterm-screen) форсятся 100%×100% чтобы заполнили git-term

#git задан как flex column (выше по файлу) — Phase 4 не трогает этот блок. Новые стили совместимы с темой через CSS-переменные (--fg-dim, --danger).
