// tmux-web — Themes panel (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - loadThemesIntoPanel  (app.js:4450)
//   - renderThemesPanel    (app.js:4487)
//   - buildThemeCard       (app.js:4623)

import { state } from '../core/state.js';
import { switchTheme } from './api.js';
import { openThemeEditor } from './editor.js';

export async function loadThemesIntoPanel(panel, themesState) {
    if (!panel) return;
    panel.innerHTML = '<div class="themes-loading">Loading themes…</div>';
    try {
        const r = await fetch('/api/themes');
        if (!r.ok) {
            throw new Error('HTTP ' + r.status);
        }
        const data = await r.json();
        const norm = {
            presets: Array.isArray(data && data.presets) ? data.presets : [],
            custom: Array.isArray(data && data.custom) ? data.custom : [],
            active: (data && typeof data.active === 'string') ? data.active : null,
        };
        themesState.data = norm;
        themesState.loaded = true;
        renderThemesPanel(panel, themesState);
    } catch (e) {
        panel.innerHTML = '';
        const err = document.createElement('div');
        err.className = 'themes-error';
        err.textContent = 'Failed to load themes: ' + (e && e.message ? e.message : String(e));
        panel.appendChild(err);
        const retry = document.createElement('button');
        retry.type = 'button';
        retry.className = 'themes-retry';
        retry.textContent = 'Retry';
        retry.addEventListener('click', () => loadThemesIntoPanel(panel, themesState));
        panel.appendChild(retry);
    }
}

export function renderThemesPanel(panel, themesState) {
    if (!panel || !themesState || !themesState.data) return;
    const data = themesState.data;
    panel.innerHTML = '';

    const presetsSection = document.createElement('section');
    presetsSection.className = 'themes-section';

    const presetsTitle = document.createElement('h3');
    presetsTitle.className = 'themes-section-title';
    presetsTitle.textContent = 'Presets';
    presetsSection.appendChild(presetsTitle);

    const presetsGrid = document.createElement('div');
    presetsGrid.className = 'theme-card-grid';
    for (const theme of data.presets) {
        const isActive = theme && theme.id === data.active;
        const card = buildThemeCard(theme, isActive, async () => {
            if (!theme || !theme.id) return;
            if (theme.id === data.active) return;
            await switchTheme(theme.id);
            if (state.activeTheme && state.activeTheme.id) {
                themesState.data.active = state.activeTheme.id;
            } else {
                themesState.data.active = theme.id;
            }
            renderThemesPanel(panel, themesState);
        });
        presetsGrid.appendChild(card);
    }
    presetsSection.appendChild(presetsGrid);
    panel.appendChild(presetsSection);

    const customSection = document.createElement('section');
    customSection.className = 'themes-section themes-section-custom';

    const customHeader = document.createElement('div');
    customHeader.className = 'themes-section-header';
    const customTitle = document.createElement('h3');
    customTitle.className = 'themes-section-title';
    customTitle.textContent = 'Custom themes';
    customHeader.appendChild(customTitle);

    const newBtn = document.createElement('button');
    newBtn.type = 'button';
    newBtn.className = 'theme-new-btn';
    newBtn.textContent = '+ New custom';
    newBtn.addEventListener('click', () => {
        openThemeEditor(null);
    });
    customHeader.appendChild(newBtn);
    customSection.appendChild(customHeader);

    const customGrid = document.createElement('div');
    customGrid.className = 'theme-card-grid';
    if (!data.custom.length) {
        const empty = document.createElement('div');
        empty.className = 'themes-empty';
        empty.textContent = 'No custom themes yet.';
        customGrid.appendChild(empty);
    } else {
        for (const theme of data.custom) {
            const isActive = theme && theme.id === data.active;
            const card = buildThemeCard(theme, isActive, async () => {
                if (!theme || !theme.id) return;
                if (theme.id === data.active) return;
                await switchTheme(theme.id);
                if (state.activeTheme && state.activeTheme.id) {
                    themesState.data.active = state.activeTheme.id;
                } else {
                    themesState.data.active = theme.id;
                }
                renderThemesPanel(panel, themesState);
            });

            const tools = document.createElement('div');
            tools.className = 'theme-card-tools';

            const editBtn = document.createElement('button');
            editBtn.type = 'button';
            editBtn.className = 'theme-card-tool';
            editBtn.title = 'Edit';
            editBtn.textContent = 'edit';
            editBtn.addEventListener('click', (ev) => {
                ev.stopPropagation();
                openThemeEditor(theme);
            });
            tools.appendChild(editBtn);

            const delBtn = document.createElement('button');
            delBtn.type = 'button';
            delBtn.className = 'theme-card-tool theme-card-tool-danger';
            delBtn.title = 'Delete';
            delBtn.textContent = 'del';
            delBtn.addEventListener('click', async (ev) => {
                ev.stopPropagation();
                if (!theme || !theme.id) return;
                if (!window.confirm(`Удалить тему "${theme.name || theme.id}"?`)) return;
                try {
                    const r = await fetch('/api/themes/' + encodeURIComponent(theme.id), {
                        method: 'DELETE',
                    });
                    if (!r.ok && r.status !== 204) {
                        const text = await r.text().catch(() => '');
                        window.alert('Failed to delete: ' + (text || r.status));
                        return;
                    }
                    themesState.loaded = false;
                    await loadThemesIntoPanel(panel, themesState);
                } catch (err) {
                    window.alert('Failed to delete: ' + (err && err.message ? err.message : err));
                }
            });
            tools.appendChild(delBtn);

            card.appendChild(tools);
            customGrid.appendChild(card);
        }
    }
    customSection.appendChild(customGrid);
    panel.appendChild(customSection);
}

export function buildThemeCard(theme, isActive, onClick) {
    const card = document.createElement('div');
    card.className = 'theme-card' + (isActive ? ' active' : '');
    if (theme && theme.id) {
        card.dataset.themeId = theme.id;
    }
    if (typeof onClick === 'function') {
        card.addEventListener('click', onClick);
    }

    const name = document.createElement('div');
    name.className = 'theme-card-name';
    name.textContent = (theme && theme.name) ? theme.name : (theme && theme.id ? theme.id : '—');
    card.appendChild(name);

    const preview = document.createElement('div');
    preview.className = 'theme-card-preview';
    const term = (theme && theme.term) ? theme.term : {};
    const swatches = [
        { key: 'background', color: term.background },
        { key: 'foreground', color: term.foreground },
        { key: 'black', color: term.black },
        { key: 'red', color: term.red },
        { key: 'green', color: term.green },
        { key: 'yellow', color: term.yellow },
        { key: 'blue', color: term.blue },
        { key: 'magenta', color: term.magenta },
        { key: 'cyan', color: term.cyan },
        { key: 'white', color: term.white },
    ];
    for (const sw of swatches) {
        const cell = document.createElement('span');
        cell.className = 'theme-card-swatch theme-card-swatch-' + sw.key;
        if (typeof sw.color === 'string' && sw.color) {
            cell.style.background = sw.color;
        }
        cell.title = sw.key + (sw.color ? ': ' + sw.color : '');
        preview.appendChild(cell);
    }
    card.appendChild(preview);

    if (isActive) {
        const badge = document.createElement('div');
        badge.className = 'theme-card-badge';
        badge.textContent = 'active';
        card.appendChild(badge);
    }

    return card;
}
