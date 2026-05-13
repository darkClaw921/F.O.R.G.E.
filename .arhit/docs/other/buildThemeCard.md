# buildThemeCard

Phase wk7.4: фабрика DOM-карточки для одной темы.

Сигнатура: buildThemeCard(theme: Theme, isActive: boolean, onClick: () => void) → HTMLElement

Возвращает .theme-card (cursor pointer, border, padding, dataset.themeId=theme.id) с дочерними:
- .theme-card-name — theme.name (fallback на theme.id или '—')
- .theme-card-preview — flex row 22px высотой из 10 цветных полосок (.theme-card-swatch):
    background, foreground, black, red, green, yellow, blue, magenta, cyan, white.
    background и foreground имеют flex 1.4 (визуально доминируют — это лицо темы).
    Если term[key] не строка — swatch остаётся прозрачным (background не выставляется).
    title=key+color для тултипа.
- isActive=true: добавляется класс .active (border-color accent + box-shadow inset accent) и .theme-card-badge 'active' в правом верхнем углу.

Click по карточке (если onClick передан) — onClick(). Никаких внутренних запросов.
