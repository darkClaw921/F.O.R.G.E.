// tmux-web — User settings API client (Phase 2 — TODO behavior feature)
//
// Клиент для эндпоинтов GET/PATCH /api/user-settings, реализованных в
// Phase 1 (tmux-web/src/user_settings.rs + handlers). Кэширует ответ в
// state.userSettings; при PATCH применяет optimistic update + rollback
// при сетевой/серверной ошибке.
//
// Контракт state.userSettings:
//   null — до первого успешного fetchUserSettings (или после ошибки);
//   object — { todo_default_plan_mode, todo_default_priority,
//     todo_default_issue_type, todo_plan_mode_suffix,
//     todo_confirm_delete, todo_confirm_promote_on_drag, ... }.
//
// Использование:
//   import { fetchUserSettings, updateUserSettings } from './user-settings-api.js';
//   await fetchUserSettings();                  // preload в bootstrap (best-effort)
//   await updateUserSettings({ todo_default_plan_mode: true });

import { state } from '../core/state.js';

const ENDPOINT = '/api/user-settings';

// GET /api/user-settings → state.userSettings.
// Возвращает объект settings при успехе или null при ошибке.
// При ошибке state.userSettings не меняется (остаётся прежним значением);
// если ранее был null — останется null, и UI должен использовать дефолты.
export async function fetchUserSettings() {
    try {
        const r = await fetch(ENDPOINT, {
            method: 'GET',
            headers: { 'Accept': 'application/json' },
        });
        if (!r.ok) {
            // eslint-disable-next-line no-console
            console.error('fetchUserSettings: HTTP ' + r.status);
            return null;
        }
        const data = await r.json();
        state.userSettings = data;
        return data;
    } catch (e) {
        // eslint-disable-next-line no-console
        console.error('fetchUserSettings: network error', e);
        return null;
    }
}

// PATCH /api/user-settings с optimistic update.
// payload — partial объект с полями, которые надо изменить.
// На успехе: state.userSettings = ответ сервера; возвращает settings.
// На ошибке: rollback state.userSettings к prev; пробрасывает Error для UI.
export async function updateUserSettings(payload) {
    const prev = state.userSettings
        ? deepClone(state.userSettings)
        : null;
    // Optimistic merge: текущий снапшот + новые поля.
    state.userSettings = Object.assign({}, state.userSettings || {}, payload || {});

    try {
        const r = await fetch(ENDPOINT, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload || {}),
        });
        if (!r.ok) {
            state.userSettings = prev;
            const text = await r.text().catch(() => '');
            throw new Error(text || ('HTTP ' + r.status));
        }
        const updated = await r.json();
        state.userSettings = updated;
        return updated;
    } catch (e) {
        // Если ошибка случилась после throw выше — prev уже восстановлен.
        // Если упал fetch/JSON-parse — тоже откатываем (мог быть мутирован выше).
        state.userSettings = prev;
        throw e;
    }
}

// Локальный deep-clone (без зависимости от structuredClone — для совместимости
// со старыми браузерами). Достаточно для плоских объектов настроек.
function deepClone(obj) {
    if (typeof structuredClone === 'function') {
        try {
            return structuredClone(obj);
        } catch (_) { /* fallback ниже */ }
    }
    return JSON.parse(JSON.stringify(obj));
}
