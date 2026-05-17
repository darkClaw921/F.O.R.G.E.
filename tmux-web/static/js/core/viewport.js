// tmux-web — viewport / responsive helpers (Phase 0 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - _mqlMobile             (app.js:667-669)
//   - isMobileViewport       (app.js:670-672)
//   - TERM_FONT_SIZE_DESKTOP (app.js:769)
//   - TERM_FONT_SIZE_MOBILE  (app.js:770)
//   - applyTerminalFontSize  (app.js:777-792)
//
// Mobile (matchMedia '(max-width: 768px)'): класс 'sidebar-open' на <body>,
// drawer-режим (off-canvas, см. style.css). Шрифт xterm на мобиле — 11px
// (стандартные 80 колонок влезают в узкий viewport), на десктопе — 13px.
//
// Зависимости: state из core/state.js (для state.term, state.fitAddon,
// state.gitTerm, state.dockerTerm, state.telescopeTerm).
//
// В Phase 0 модуль ещё НЕ подключен к index.html; готов к импорту в Phase 1.

import { state } from './state.js';

// matchMedia-инстанс на module-level, чтобы не плодить listeners.
export const _mqlMobile = (typeof window.matchMedia === 'function')
    ? window.matchMedia('(max-width: 768px)')
    : null;

export function isMobileViewport() {
    return !!(_mqlMobile && _mqlMobile.matches);
}

// -------------------------------------------------------------------------
// Mobile font scaling для xterm/TUI (A4)
//
// На мобиле уменьшаем шрифт всех xterm-инстансов (основной + git/docker/
// telescope), чтобы стандартные 80 колонок влезали в узкий viewport. После
// изменения fontSize вызываем fit() — он пересчитает cols/rows и xterm
// авто-эмиттит onResize → sendResize в PTY (см. attach в createTuiTab).
// -------------------------------------------------------------------------
export const TERM_FONT_SIZE_DESKTOP = 13;
export const TERM_FONT_SIZE_MOBILE = 11;

/**
 * Применяет нужный fontSize ко всем существующим xterm-инстансам и делает
 * fit(). Безопасен на любом этапе bootstrap — пропускает несуществующие
 * либо ещё не attached терминалы (FitAddon бросает, если term._core === undefined).
 */
export function applyTerminalFontSize() {
    const size = isMobileViewport() ? TERM_FONT_SIZE_MOBILE : TERM_FONT_SIZE_DESKTOP;
    // Основной терминал
    if (state.term && state.term.options && state.term.options.fontSize !== size) {
        try { state.term.options.fontSize = size; } catch (_) {}
        try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
    }
    // TUI-инстансы: каждый — это TuiTab.state с .term и .fit (FitAddon)
    const tuis = [state.gitTerm, state.dockerTerm, state.telescopeTerm];
    tuis.forEach((t) => {
        if (!t || !t.term || !t.term.options) return;
        if (t.term.options.fontSize === size) return;
        try { t.term.options.fontSize = size; } catch (_) {}
        try { t.fit && t.fit.fit(); } catch (_) {}
    });
}
