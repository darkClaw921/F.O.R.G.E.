# tmux-web/static/js/core/viewport.js

Viewport / responsive helpers (Phase 0 ES Modules refactor).

## Назначение
1:1 копии viewport-helper'ов из IIFE `tmux-web/static/app.js` (строки 667-672, 769-792).

## Экспорты
- `_mqlMobile: MediaQueryList | null` — module-level matchMedia('(max-width: 768px)'). null если matchMedia недоступен (старые браузеры).
- `isMobileViewport(): boolean` — true если viewport ≤ 768px. Используется как guard в `toggleSidebar`, `restoreSidebarState`, `applySidebarCollapsed`, terminal scaling.
- `TERM_FONT_SIZE_DESKTOP: 13` — fontSize для xterm на десктопе.
- `TERM_FONT_SIZE_MOBILE: 11` — fontSize для xterm на мобиле (чтобы 80 колонок влезали).
- `applyTerminalFontSize(): void` — пробегается по `state.term` + `state.gitTerm` / `state.dockerTerm` / `state.telescopeTerm`, выставляет нужный fontSize и вызывает `fit()` (xterm auto-emit onResize → sendResize в PTY).

## Зависимости
- `import { state } from './state.js'` — для `state.term, state.fitAddon, state.gitTerm, state.dockerTerm, state.telescopeTerm`.

## Использование (Phase 1)
- `isMobileViewport()` — sidebar/mobile.js, settings/modal.js.
- `_mqlMobile.addEventListener('change', ...)` — bootstrap.js для реакции на смену viewport.
- `applyTerminalFontSize()` — bootstrap.js + reaction на mql change.

## Ограничения
- `applyTerminalFontSize` безопасен на любом этапе bootstrap — пропускает несуществующие/неmount-нутые терминалы (try/catch вокруг fit()).
- В Phase 0 модуль ещё не подключен; legacy app.js содержит свои копии.
