# tmux-web/static/js/themes/api.js

Phase 1. Themes runtime+API: applyTheme (CSS-vars на :root + state.term.options.theme через mapTermTheme + state.activeTheme), switchTheme (PATCH /api/themes/active + GET + applyTheme), loadActiveThemeOrNull. THEME_UI_KEYS (11) / THEME_TERM_BASE_KEYS (4) / THEME_TERM_ANSI_KEYS (16) / THEME_TERM_KEYS (combined). HEX_COLOR_RE, normalizeHex (fallback #000000), cloneThemeColors (deep clone+normalize), validateDraft, buildThemePayload (для POST/PUT custom).
