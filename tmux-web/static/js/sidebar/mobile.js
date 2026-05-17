// tmux-web — Sidebar collapse + mobile drawer (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - applySidebarCollapsed (app.js:683)
//   - setMobileSidebarOpen  (app.js:709)
//   - toggleSidebar         (app.js:718)
//   - restoreSidebarState   (app.js:730)

import { state } from '../core/state.js';
import { $layout, $btnSidebarToggle } from '../core/dom.js';
import { isMobileViewport } from '../core/viewport.js';
import { scheduleResizeFromTerm } from '../terminal/xterm.js';

export function applySidebarCollapsed(collapsed) {
    if (!$layout) return;
    state.sidebarCollapsed = !!collapsed;
    $layout.classList.toggle('sidebar-collapsed', state.sidebarCollapsed);
    if ($btnSidebarToggle) {
        $btnSidebarToggle.setAttribute('aria-pressed', String(state.sidebarCollapsed));
        $btnSidebarToggle.title = state.sidebarCollapsed
            ? 'Показать сайдбар (Cmd/Ctrl+B)'
            : 'Скрыть сайдбар (Cmd/Ctrl+B)';
    }
    // Refit активного xterm после завершения CSS-transition.
    setTimeout(() => {
        try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
        try { scheduleResizeFromTerm && scheduleResizeFromTerm(); } catch (_) {}
        const tuis = [state.gitTerm, state.dockerTerm, state.telescopeTerm];
        tuis.forEach((t) => {
            if (t && t.fit) { try { t.fit.fit(); } catch (_) {} }
        });
    }, 200);
}

export function setMobileSidebarOpen(open) {
    const isOpen = !!open;
    document.body.classList.toggle('sidebar-open', isOpen);
    if ($btnSidebarToggle) {
        $btnSidebarToggle.setAttribute('aria-pressed', String(isOpen));
        $btnSidebarToggle.title = isOpen ? 'Закрыть меню' : 'Открыть меню';
    }
}

export function toggleSidebar() {
    if (isMobileViewport()) {
        const willOpen = !document.body.classList.contains('sidebar-open');
        setMobileSidebarOpen(willOpen);
        return;
    }
    applySidebarCollapsed(!state.sidebarCollapsed);
    try {
        localStorage.setItem('forge.sidebarCollapsed', state.sidebarCollapsed ? '1' : '0');
    } catch (_) { /* privacy mode — игнор */ }
}

export function restoreSidebarState() {
    if (isMobileViewport()) {
        setMobileSidebarOpen(false);
        state.sidebarCollapsed = false;
        return;
    }
    let collapsed = false;
    try {
        collapsed = localStorage.getItem('forge.sidebarCollapsed') === '1';
    } catch (_) { /* privacy mode — дефолт false */ }
    applySidebarCollapsed(collapsed);
}
