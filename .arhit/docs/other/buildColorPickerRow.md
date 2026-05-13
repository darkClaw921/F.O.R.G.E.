# buildColorPickerRow

Phase 5: фабрика DOM-строки цветового пикера для редактора тем.

Сигнатура: buildColorPickerRow(def: {key, label}, initialHex: string, onChange: (hex)=>void, compact?: boolean) → { el: HTMLElement, setValue(hex: string): void }

Возвращает .theme-editor-row.theme-editor-color-row[.theme-editor-color-row-compact] с:
- .theme-editor-row-label — label.text = def.label
- .theme-editor-color-pair:
  - <input type=color> класс .theme-editor-color-input — нативный picker
  - <input type=text> класс .theme-editor-hex-input — hex (#rrggbb), maxLength=7

Двусторонняя синхронизация:
- color → text: input event на color-picker, всегда валидный → text.value = color.value, .invalid снимается, onChange(v).
- text → color: input event на text-input, проверяется через HEX_COLOR_RE. Валид → color.value = v, .invalid снимается, onChange(v). Невалид → text.classList.add('invalid') (border var(--danger)), onChange НЕ вызывается, color picker не меняется.
- На blur невалидного hex — text.value откатывается к color.value (последнее валидное), .invalid снимается.

setValue(hex):
- Programmatically обновляет оба инпута на normalizeHex(hex). Используется при duplicate-from-preset (applyPresetToDraft) — без вызова onChange.

Параметр compact (true): уменьшает шрифт label и размеры пикера; используется для ANSI-сетки 4×4. Иначе — обычный layout для UI и base term.

Зависимости: HEX_COLOR_RE, normalizeHex.
