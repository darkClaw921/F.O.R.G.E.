# initTerminal

Инициализация xterm.js Terminal (tmux-web/static/app.js, обновлён в Phase 3 wk7).

## Сигнатура (Phase 3 wk7)
initTerminal(termTheme) — теперь принимает xterm ITheme (результат mapTermTheme). До Phase 3 был initTerminal() без аргументов с hard-coded theme.

## Что делает
1. Берёт глобалы из CDN: window.Terminal, window.FitAddon.FitAddon, window.WebLinksAddon.WebLinksAddon. Если не загружены — console.error и return.
2. Создаёт new Terminal({ cursorBlink: true, fontFamily: ui-monospace..., fontSize: 13, scrollback: 5000, allowProposedApi: true, theme: termTheme || fallbackTheme }).
3. fallbackTheme = { background:#000000, foreground:#d8dee9, cursor:#d8dee9, selectionBackground:#3a4356 } — используется только при offline (loadActiveThemeOrNull вернул null).
4. Создаёт fitAddon, webLinksAddon, loadAddon-ит их в term, term.open().
5. fitAddon.fit() в try/catch (контейнер может быть 0×0).
6. term.onData → state.ws.send(state.encoder.encode(data)) — пользовательский ввод в PTY.
7. term.onResize → sendResize(cols, rows) — JSON control message в WS.
8. Сохраняет в state.term, state.fitAddon, state.webLinksAddon.
9. ResizeObserver на  → fitAddon.fit() (на изменение sidebar/window/font).
10. window resize listener — страховка для старых браузеров.

## Параметры
termTheme: xterm ITheme | null. null → fallback. Не-null — структура с background/foreground/cursor/selectionBackground/black/.../white/brightBlack/.../brightWhite.

## Связанные
- bootstrap → вызывает с результатом loadActiveThemeOrNull.
- state.term, state.fitAddon — заполняются здесь.
- sendResize, applyTheme — используют state.term.
- xterm.js 5.3.0 + addon-fit 0.8.0 + addon-web-links 0.9.0 — через CDN.
