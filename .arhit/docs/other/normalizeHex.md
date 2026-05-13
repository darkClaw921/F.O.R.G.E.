# normalizeHex

Phase 5: нормализация hex-цвета для редактора тем.

Сигнатура: normalizeHex(value: any, fallback?: string) → string

- Если value — строка и проходит HEX_COLOR_RE (#[0-9a-fA-F]{6}) → возвращает value.toLowerCase().
- Иначе → возвращает fallback (или '#000000' если не задан).

Используется в cloneThemeColors (защита от undefined/невалид-полей при load темы) и в buildColorPickerRow.setValue (programmatic обновление пикера).
