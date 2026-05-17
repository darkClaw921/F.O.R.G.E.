// tmux-web — Healthz + remote-mode flag (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - loadHealthz   (app.js:6225)
//   - isRemoteMode  (app.js:6251)
//
// Зависимости: state из core/state.js.
// Используется bootstrap (вызов loadHealthz до initTerminal) и многими
// модулями (sidebar/api/ws/...) для feature-toggle remote_mode.

import { state } from '../core/state.js';

/**
 * Phase 5 — GET /healthz; пишет state.remoteMode и state.serverVersion.
 *
 * Контракт: эндпоинт доступен без Bearer-auth (см. auth::is_path_excluded
 * на бэке) и отдаёт { status, remote_mode, version }. Если запрос упал —
 * считаем remote_mode=false (legacy-friendly fallback), но всё равно
 * выставляем healthzLoaded=true чтобы остальной bootstrap продолжился.
 */
export async function loadHealthz() {
    try {
        const r = await fetch('/healthz', { headers: { 'Accept': 'application/json' } });
        if (!r.ok) {
            console.warn('GET /healthz failed:', r.status);
            state.remoteMode = false;
            state.healthzLoaded = true;
            return;
        }
        const data = await r.json();
        state.remoteMode = !!data.remote_mode;
        state.serverVersion = typeof data.version === 'string' ? data.version : null;
        state.healthzLoaded = true;
    } catch (e) {
        console.warn('loadHealthz failed:', e);
        state.remoteMode = false;
        state.healthzLoaded = true;
    }
}

/**
 * Phase 5 — true если frontend должен рендерить новый UI (origin-табы,
 * Settings → Remote servers tab, кнопка add-remote, и т.п.).
 * Используется как guard в renderSidebar, openSettingsModal и API-helper'ах.
 * При false поведение фронта побитово совпадает с legacy.
 */
export function isRemoteMode() {
    return state.remoteMode === true;
}
