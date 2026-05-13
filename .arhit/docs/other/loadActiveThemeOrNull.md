# loadActiveThemeOrNull

Bootstrap-загрузка активной темы до инициализации xterm (tmux-web/static/app.js, Phase 3 wk7).

## Что делает
async loadActiveThemeOrNull() — вызывается из bootstrap() ДО initTerminal:
1. fetch GET /api/themes/active.
2. Если ответ не ok — console.warn и возвращает null (initTerminal применит fallback-палитру).
3. Парсит JSON, вызывает applyTheme(theme) — это применяет CSS-переменные на :root СРАЗУ. xterm-ветка внутри applyTheme пропускается т.к. state.term ещё null.
4. Возвращает mapTermTheme(theme.term) — для передачи в initTerminal({theme: ...}).

## Зачем нужно ДО initTerminal
xterm.js рендерит background-canvas при первом open(). Если создать Terminal с дефолтной темой, а потом присвоить options.theme — глифы перерисуются с новыми цветами, но background-canvas останется от старой темы до следующего полного перерисова. Поэтому термальная палитра должна попасть в конструктор new Terminal({theme: ...}).

## Возвращает
xterm ITheme | null. null означает: API недоступен или вернул не-2xx; вызывающий передаст null в initTerminal, который применит fallback-палитру (background:#000, foreground:#d8dee9 — соответствует историческому поведению до Phase 3).

## Обработка ошибок
- Не-2xx ответ → console.warn + return null (НЕ alert, чтобы не блокировать загрузку при offline-разработке).
- Сетевой сбой (catch) → console.warn + return null.

## Связанные
- bootstrap() — единственный вызывающий.
- initTerminal(termTheme) — принимает результат.
- applyTheme — применяет CSS-секцию.
- /api/themes/active — themes.rs::get_active_theme.
