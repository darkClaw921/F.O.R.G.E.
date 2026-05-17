// tmux-web — xterm.js theme mapper (Phase 1 ES Modules refactor)
//
// 1:1 копия mapTermTheme из IIFE `tmux-web/static/app.js` (строки 362-388).
//
// Маппит нашу TermColors-палитру (camelCase из serde) в xterm.js ITheme.
// Единственная переименовка: selection → selectionBackground.
// Остальные поля совпадают (foreground/background/cursor/black/red/green/
// yellow/blue/magenta/cyan/white/brightBlack/.../brightWhite).
//
// Pure leaf-module — никаких импортов.

/**
 * Маппит нашу TermColors-палитру в xterm.js ITheme.
 * Возвращает новый объект, безопасно присваиваемый в term.options.theme.
 */
export function mapTermTheme(t) {
    if (!t) return {};
    return {
        foreground: t.foreground,
        background: t.background,
        cursor: t.cursor,
        // xterm.js использует selectionBackground (ранее selection); наш
        // бэкенд хранит просто `selection` для краткости, переименовываем тут.
        selectionBackground: t.selection,
        black: t.black,
        red: t.red,
        green: t.green,
        yellow: t.yellow,
        blue: t.blue,
        magenta: t.magenta,
        cyan: t.cyan,
        white: t.white,
        brightBlack: t.brightBlack,
        brightRed: t.brightRed,
        brightGreen: t.brightGreen,
        brightYellow: t.brightYellow,
        brightBlue: t.brightBlue,
        brightMagenta: t.brightMagenta,
        brightCyan: t.brightCyan,
        brightWhite: t.brightWhite,
    };
}
