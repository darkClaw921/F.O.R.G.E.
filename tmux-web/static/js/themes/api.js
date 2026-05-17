// tmux-web — Themes runtime + API (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - applyTheme       (app.js:394)
//   - switchTheme      (app.js:436)
//   - loadActiveThemeOrNull (app.js:6199)
//   - THEME_UI_KEYS / THEME_TERM_BASE_KEYS / THEME_TERM_ANSI_KEYS /
//     THEME_TERM_KEYS / HEX_COLOR_RE (app.js:4708-4760)
//   - normalizeHex     (app.js:4766)
//   - cloneThemeColors (app.js:4778)
//   - validateDraft    (app.js:5344)
//   - buildThemePayload (app.js:5370)

import { state } from '../core/state.js';
import { mapTermTheme } from '../terminal/theme-mapper.js';

export function applyTheme(theme) {
    if (!theme) return;
    const ui = theme.ui || {};
    const cssMap = {
        bg: '--bg',
        bgElev: '--bg-elev',
        fg: '--fg',
        fgDim: '--fg-dim',
        border: '--border',
        accent: '--accent',
        warn: '--warn',
        danger: '--danger',
        p0: '--p0',
        p1: '--p1',
        p2: '--p2',
    };
    const root = document.documentElement;
    for (const [k, cssVar] of Object.entries(cssMap)) {
        const v = ui[k];
        if (typeof v === 'string' && v.length > 0) {
            root.style.setProperty(cssVar, v);
        }
    }
    if (state.term && theme.term) {
        try {
            state.term.options.theme = mapTermTheme(theme.term);
        } catch (e) {
            console.warn('xterm options.theme assignment failed', e);
        }
    }
    state.activeTheme = theme;
}

export async function switchTheme(id) {
    try {
        const patchResp = await fetch('/api/themes/active', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ id }),
        });
        if (!patchResp.ok) {
            const text = await patchResp.text().catch(() => '');
            window.alert('Failed to switch theme: ' + (text || patchResp.status));
            return;
        }
        const getResp = await fetch('/api/themes/active');
        if (!getResp.ok) {
            window.alert('Failed to fetch active theme: ' + getResp.status);
            return;
        }
        const theme = await getResp.json();
        applyTheme(theme);
    } catch (e) {
        window.alert('Failed to switch theme: ' + e.message);
    }
}

export async function loadActiveThemeOrNull() {
    try {
        const resp = await fetch('/api/themes/active', { headers: { 'Accept': 'application/json' } });
        if (!resp.ok) {
            console.warn('GET /api/themes/active failed:', resp.status);
            return null;
        }
        const theme = await resp.json();
        applyTheme(theme);
        return theme.term ? mapTermTheme(theme.term) : null;
    } catch (e) {
        console.warn('loadActiveThemeOrNull failed:', e);
        return null;
    }
}

export const THEME_UI_KEYS = [
    { key: 'bg',      label: 'Background' },
    { key: 'bgElev',  label: 'Background (elev)' },
    { key: 'fg',      label: 'Foreground' },
    { key: 'fgDim',   label: 'Foreground (dim)' },
    { key: 'border',  label: 'Border' },
    { key: 'accent',  label: 'Accent' },
    { key: 'warn',    label: 'Warning' },
    { key: 'danger',  label: 'Danger' },
    { key: 'p0',      label: 'Priority P0' },
    { key: 'p1',      label: 'Priority P1' },
    { key: 'p2',      label: 'Priority P2' },
];

export const THEME_TERM_BASE_KEYS = [
    { key: 'foreground', label: 'Foreground' },
    { key: 'background', label: 'Background' },
    { key: 'cursor',     label: 'Cursor' },
    { key: 'selection',  label: 'Selection' },
];

export const THEME_TERM_ANSI_KEYS = [
    { key: 'black',         label: 'black' },
    { key: 'red',           label: 'red' },
    { key: 'green',         label: 'green' },
    { key: 'yellow',        label: 'yellow' },
    { key: 'blue',          label: 'blue' },
    { key: 'magenta',       label: 'magenta' },
    { key: 'cyan',          label: 'cyan' },
    { key: 'white',         label: 'white' },
    { key: 'brightBlack',   label: 'br.black' },
    { key: 'brightRed',     label: 'br.red' },
    { key: 'brightGreen',   label: 'br.green' },
    { key: 'brightYellow',  label: 'br.yellow' },
    { key: 'brightBlue',    label: 'br.blue' },
    { key: 'brightMagenta', label: 'br.magenta' },
    { key: 'brightCyan',    label: 'br.cyan' },
    { key: 'brightWhite',   label: 'br.white' },
];

export const THEME_TERM_KEYS = THEME_TERM_BASE_KEYS.concat(THEME_TERM_ANSI_KEYS);

export const HEX_COLOR_RE = /^#[0-9a-fA-F]{6}$/;

export function normalizeHex(value, fallback) {
    if (typeof value === 'string' && HEX_COLOR_RE.test(value)) {
        return value.toLowerCase();
    }
    return fallback || '#000000';
}

export function cloneThemeColors(theme) {
    const srcUi = (theme && theme.ui) ? theme.ui : {};
    const srcTerm = (theme && theme.term) ? theme.term : {};
    const ui = {};
    for (const { key } of THEME_UI_KEYS) {
        ui[key] = normalizeHex(srcUi[key], '#000000');
    }
    const term = {};
    for (const { key } of THEME_TERM_KEYS) {
        term[key] = normalizeHex(srcTerm[key], '#000000');
    }
    return { ui, term };
}

export function validateDraft(draft) {
    const trimmed = (draft.name || '').trim();
    if (!trimmed) {
        return { ok: false, error: 'Name is required.' };
    }
    for (const { key, label } of THEME_UI_KEYS) {
        if (!HEX_COLOR_RE.test(draft.ui[key] || '')) {
            return { ok: false, error: `UI / ${label}: invalid hex color.` };
        }
    }
    for (const { key, label } of THEME_TERM_KEYS) {
        if (!HEX_COLOR_RE.test(draft.term[key] || '')) {
            return { ok: false, error: `Terminal / ${label}: invalid hex color.` };
        }
    }
    return { ok: true };
}

export function buildThemePayload(draft, isEdit) {
    return {
        id: isEdit ? draft.id : '',
        name: (draft.name || '').trim(),
        kind: 'custom',
        ui: { ...draft.ui },
        term: { ...draft.term },
    };
}
