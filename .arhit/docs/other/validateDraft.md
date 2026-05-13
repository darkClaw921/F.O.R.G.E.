# validateDraft

Phase 5: валидация черновика темы перед save в openThemeEditor.

Сигнатура: validateDraft(draft: {name, ui, term}) → { ok: true } | { ok: false, error: string }

Проверяет:
1. draft.name.trim() не пустой → 'Name is required.'
2. Для каждого ключа из THEME_UI_KEYS — draft.ui[key] валидный hex по HEX_COLOR_RE → 'UI / {label}: invalid hex color.'
3. Для каждого ключа из THEME_TERM_KEYS (base 4 + ANSI 16) — draft.term[key] валидный hex → 'Terminal / {label}: invalid hex color.'

Используется кнопкой Save: при ошибке alert + отмена запроса. В норме (пользователь не вмешивался напрямую) draft всегда валиден, т.к. buildColorPickerRow гарантирует rollback на blur.
