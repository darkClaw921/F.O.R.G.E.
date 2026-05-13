# mapTermTheme

Маппер TermColors → xterm.js ITheme (tmux-web/static/app.js, Phase 3 wk7).

## Что делает
mapTermTheme(t) приводит структуру TermColors из бэкенда (camelCase) к формату xterm.js ITheme. Возвращает новый объект, безопасный для присвоения в term.options.theme или для передачи в new Terminal({theme: ...}).

## Маппинг ключей
- foreground/background/cursor → как есть.
- selection → **selectionBackground** (единственная переименовка: xterm 5.x использует selectionBackground; до 5.x было selection).
- ANSI 8-bit: black, red, green, yellow, blue, magenta, cyan, white → как есть.
- ANSI bright: brightBlack, brightRed, brightGreen, brightYellow, brightBlue, brightMagenta, brightCyan, brightWhite → как есть. Бэкенд сериализует через serde rename_all=camelCase, поэтому фронт получает уже brightBlack, snake-case→camel конверсия не нужна.

## Параметры
t: TermColors из theme.term (после JSON.parse от GET /api/themes/active). Если t=null — возвращает {} (xterm проигнорирует и оставит дефолты).

## Связанные
- applyTheme — единственный вызывающий внутри runtime.
- initTerminal — принимает результат mapTermTheme как initial theme.
- themes.rs::TermColors — серверный аналог.

## Бизнес-логика
Поскольку xterm.js рендерит background сразу при open(), на bootstrap-стадии mapTermTheme должен передаваться в конструктор Terminal — присвоение options.theme после open пересчитает только глифы, оставив background-canvas от старой темы.
