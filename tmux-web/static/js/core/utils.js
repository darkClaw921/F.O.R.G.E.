// tmux-web — pure helper utilities (Phase 0 ES Modules refactor)
//
// 1:1 копии функций из IIFE `tmux-web/static/app.js`:
//   - escapeHtml         (app.js:4408)
//   - escapeAttr         (app.js:5852)
//   - escapeText         (app.js:5855)
//   - buildModalOverlay  (app.js:5582)
//   - detectClientOS     (app.js:2019)
//   - copyToClipboardSafe(app.js:2034)
//   - fallbackCopy       (app.js:2041)
//
// Никаких импортов извне — pure helpers. В Phase 0 модуль ещё НЕ подключен
// к index.html; готов к импорту из feature-модулей в Phase 1.

/**
 * Phase 5 — Безопасное экранирование строки для вставки в innerHTML
 * (используется в openEditRemoteRow для подстановки label в input value).
 */
export function escapeHtml(s) {
    return String(s == null ? '' : s)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

export function escapeAttr(s) {
    return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');
}

export function escapeText(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * Создаёт overlay + базовый style hook. CSS — в style.css секция Modals.
 */
export function buildModalOverlay() {
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay';
    return overlay;
}

/**
 * Определение клиентской ОС по navigator.platform + userAgent.
 * Возвращает 'mac' | 'windows' | 'linux' | null.
 */
export function detectClientOS() {
    const nav = (typeof navigator !== 'undefined') ? navigator : null;
    if (!nav) return null;
    const ua = (nav.userAgent || '').toLowerCase();
    const platform = (nav.platform || '').toLowerCase();
    if (platform.includes('mac') || ua.includes('mac os x') || ua.includes('macintosh')) return 'mac';
    if (platform.includes('win') || ua.includes('windows')) return 'windows';
    if (platform.includes('linux') || ua.includes('linux') || ua.includes('x11')) return 'linux';
    return null;
}

/**
 * Копирует строку в буфер. Использует Clipboard API если доступен,
 * fallback — скрытый textarea + execCommand. Возвращает Promise<boolean>.
 */
export function copyToClipboardSafe(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
        return navigator.clipboard.writeText(text).then(() => true).catch(() => fallbackCopy(text));
    }
    return Promise.resolve(fallbackCopy(text));
}

export function fallbackCopy(text) {
    try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
    } catch (_) {
        return false;
    }
}
