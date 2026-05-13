# cloneThemeColors

Phase 5: глубокое копирование ui/term из объекта Theme с гарантией наличия ВСЕХ ключей.

Сигнатура: cloneThemeColors(theme?: Theme | null) → { ui: object, term: object }

- Для каждого ключа из THEME_UI_KEYS (11) → ui[key] = normalizeHex(theme?.ui?.[key], '#000000').
- Для каждого ключа из THEME_TERM_KEYS (20) → term[key] = normalizeHex(theme?.term?.[key], '#000000').

Используется в openThemeEditor для baseline draft (как из themeOrNull в edit-режиме, так и из state.activeTheme в create-режиме) и в applyPresetToDraft (когда юзер выбирает 'Duplicate from preset').
