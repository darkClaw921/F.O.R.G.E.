# applyTheme

Frontend runtime для применения темы (tmux-web/static/app.js, Phase 3 wk7).

## Что делает
applyTheme(theme) применяет полученную с бэкенда тему в две точки:
1. **CSS-переменные на :root** — для каждого ключа theme.ui (camelCase) ставит соответствующую --kebab-case переменную через document.documentElement.style.setProperty. 11 themable ключей: bg→--bg, bgElev→--bg-elev, fg→--fg, fgDim→--fg-dim, border→--border, accent→--accent, warn→--warn, danger→--danger, p0→--p0, p1→--p1, p2→--p2.
2. **xterm.js options.theme** — если state.term существует, обновляет term.options.theme через mapTermTheme(theme.term). xterm 5.x поддерживает горячую смену темы через присвоение options.theme; цвета пересчитываются в следующем кадре рендера.
3. **state.activeTheme** — сохраняет тему для повторного применения и для Phase 5 (live preview редактора).

## Параметры
theme: {id?, name?, kind?, ui?, term?} — структура из GET /api/themes/active. Любое поле необязательно: пустые строки в ui игнорируются, отсутствующий term не трогает терминал.

## Защита от ошибок
- Защита от theme=null/undefined.
- Игнор пустых строк в ui (typeof v === 'string' && v.length > 0).
- try/catch на присвоение options.theme — на случай если xterm-инстанс в не-готовом состоянии.

## Связанные
- mapTermTheme — маппит наши имена в xterm ITheme.
- switchTheme — переключает active на сервере и применяет.
- loadActiveThemeOrNull — bootstrap-загрузка.
- state.activeTheme — текущая применённая тема.
- style.css :root — 11 переменных задаются с дефолтами (Phase 2 wk7).
- themes.rs (backend) — отдаёт Theme в camelCase через serde.

## Бизнес-логика
applyTheme вызывается:
- на bootstrap из loadActiveThemeOrNull (CSS применяется до new Terminal).
- из switchTheme после PATCH+GET active.
- (будущее, Phase 5) из редактора кастомной темы для live preview.
