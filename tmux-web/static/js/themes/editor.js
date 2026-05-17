// tmux-web — Theme editor (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - openThemeEditor          (app.js:4798)
//   - buildColorPickerRow      (app.js:5107)
//   - buildLivePreviewContainer (app.js:5187)

import { state } from '../core/state.js';
import { buildModalOverlay } from '../core/utils.js';
import {
    THEME_UI_KEYS, THEME_TERM_BASE_KEYS, THEME_TERM_ANSI_KEYS, THEME_TERM_KEYS,
    HEX_COLOR_RE, normalizeHex, cloneThemeColors,
    validateDraft, buildThemePayload,
} from './api.js';
import { loadThemesIntoPanel } from './panel.js';

export function openThemeEditor(themeOrNull) {
    const isEdit = !!(themeOrNull && themeOrNull.id);
    const baseline = isEdit
        ? themeOrNull
        : (state.activeTheme || null);
    const cloned = cloneThemeColors(baseline);
    const draft = {
        id: isEdit ? themeOrNull.id : '',
        name: isEdit ? (themeOrNull.name || '') : '',
        ui: cloned.ui,
        term: cloned.term,
    };

    let presets = [];

    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card theme-editor-modal';

    const header = document.createElement('div');
    header.className = 'theme-editor-header';
    const title = document.createElement('h2');
    title.textContent = isEdit
        ? `Edit theme: ${themeOrNull.name || themeOrNull.id}`
        : 'New custom theme';
    header.appendChild(title);
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'theme-editor-close';
    closeBtn.setAttribute('aria-label', 'Close');
    closeBtn.textContent = '×';
    header.appendChild(closeBtn);
    card.appendChild(header);

    const body = document.createElement('div');
    body.className = 'theme-editor-body';

    const metaRow = document.createElement('div');
    metaRow.className = 'theme-editor-section theme-editor-meta';

    const nameWrap = document.createElement('label');
    nameWrap.className = 'theme-editor-row';
    const nameLbl = document.createElement('span');
    nameLbl.className = 'theme-editor-row-label';
    nameLbl.textContent = 'Name';
    nameWrap.appendChild(nameLbl);
    const nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'theme-editor-name';
    nameInput.placeholder = 'My theme';
    nameInput.value = draft.name;
    nameInput.addEventListener('input', () => {
        draft.name = nameInput.value;
    });
    nameWrap.appendChild(nameInput);
    metaRow.appendChild(nameWrap);

    const dupWrap = document.createElement('label');
    dupWrap.className = 'theme-editor-row';
    const dupLbl = document.createElement('span');
    dupLbl.className = 'theme-editor-row-label';
    dupLbl.textContent = 'Duplicate from preset';
    dupWrap.appendChild(dupLbl);
    const dupSelect = document.createElement('select');
    dupSelect.className = 'theme-editor-duplicate';
    const dupDefault = document.createElement('option');
    dupDefault.value = '';
    dupDefault.textContent = 'From scratch';
    dupSelect.appendChild(dupDefault);
    dupWrap.appendChild(dupSelect);
    metaRow.appendChild(dupWrap);
    body.appendChild(metaRow);

    const uiSection = document.createElement('section');
    uiSection.className = 'theme-editor-section';
    const uiTitle = document.createElement('h3');
    uiTitle.className = 'theme-editor-section-title';
    uiTitle.textContent = 'UI colors';
    uiSection.appendChild(uiTitle);
    const uiGrid = document.createElement('div');
    uiGrid.className = 'theme-editor-ui-grid';
    const uiRefs = {};
    for (const def of THEME_UI_KEYS) {
        const row = buildColorPickerRow(def, draft.ui[def.key], (newHex) => {
            draft.ui[def.key] = newHex;
            updatePreview();
        });
        uiRefs[def.key] = row;
        uiGrid.appendChild(row.el);
    }
    uiSection.appendChild(uiGrid);
    body.appendChild(uiSection);

    const termSection = document.createElement('section');
    termSection.className = 'theme-editor-section';
    const termTitle = document.createElement('h3');
    termTitle.className = 'theme-editor-section-title';
    termTitle.textContent = 'Terminal colors';
    termSection.appendChild(termTitle);
    const termBaseGrid = document.createElement('div');
    termBaseGrid.className = 'theme-editor-term-base-grid';
    const termRefs = {};
    for (const def of THEME_TERM_BASE_KEYS) {
        const row = buildColorPickerRow(def, draft.term[def.key], (newHex) => {
            draft.term[def.key] = newHex;
            updatePreview();
        });
        termRefs[def.key] = row;
        termBaseGrid.appendChild(row.el);
    }
    termSection.appendChild(termBaseGrid);
    const ansiTitle = document.createElement('div');
    ansiTitle.className = 'theme-editor-ansi-title';
    ansiTitle.textContent = 'ANSI palette';
    termSection.appendChild(ansiTitle);
    const ansiGrid = document.createElement('div');
    ansiGrid.className = 'theme-editor-ansi-grid';
    for (const def of THEME_TERM_ANSI_KEYS) {
        const row = buildColorPickerRow(def, draft.term[def.key], (newHex) => {
            draft.term[def.key] = newHex;
            updatePreview();
        }, true);
        termRefs[def.key] = row;
        ansiGrid.appendChild(row.el);
    }
    termSection.appendChild(ansiGrid);
    body.appendChild(termSection);

    const previewSection = document.createElement('section');
    previewSection.className = 'theme-editor-section theme-editor-preview-section';
    const previewTitle = document.createElement('h3');
    previewTitle.className = 'theme-editor-section-title';
    previewTitle.textContent = 'Live preview';
    previewSection.appendChild(previewTitle);
    const previewContainer = buildLivePreviewContainer();
    previewSection.appendChild(previewContainer.el);
    body.appendChild(previewSection);

    card.appendChild(body);

    const footer = document.createElement('div');
    footer.className = 'modal-actions theme-editor-actions';
    const cancelBtn = document.createElement('button');
    cancelBtn.type = 'button';
    cancelBtn.textContent = 'Cancel';
    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    footer.appendChild(cancelBtn);
    footer.appendChild(saveBtn);
    card.appendChild(footer);

    overlay.appendChild(card);
    document.body.appendChild(overlay);

    function updatePreview() {
        previewContainer.update(draft);
    }

    function applyPresetToDraft(preset) {
        if (!preset) return;
        const cloned = cloneThemeColors(preset);
        draft.ui = cloned.ui;
        draft.term = cloned.term;
        for (const def of THEME_UI_KEYS) {
            uiRefs[def.key].setValue(draft.ui[def.key]);
        }
        for (const def of THEME_TERM_KEYS) {
            termRefs[def.key].setValue(draft.term[def.key]);
        }
        if (!draft.name.trim()) {
            draft.name = `Copy of ${preset.name || preset.id || 'preset'}`;
            nameInput.value = draft.name;
        }
        updatePreview();
    }

    updatePreview();

    fetch('/api/themes')
        .then((r) => r.ok ? r.json() : null)
        .then((data) => {
            if (!data || !Array.isArray(data.presets)) return;
            presets = data.presets;
            for (const p of presets) {
                const opt = document.createElement('option');
                opt.value = p.id || '';
                opt.textContent = p.name || p.id || '—';
                dupSelect.appendChild(opt);
            }
        })
        .catch(() => { /* dropdown останется только с "From scratch" */ });

    dupSelect.addEventListener('change', () => {
        const id = dupSelect.value;
        if (!id) return;
        const preset = presets.find((p) => p && p.id === id);
        if (preset) applyPresetToDraft(preset);
        dupSelect.value = '';
    });

    const close = () => overlay.remove();
    closeBtn.addEventListener('click', close);
    cancelBtn.addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });

    saveBtn.addEventListener('click', async () => {
        const result = validateDraft(draft);
        if (!result.ok) {
            window.alert(result.error);
            return;
        }
        const payload = buildThemePayload(draft, isEdit);
        saveBtn.disabled = true;
        cancelBtn.disabled = true;
        try {
            let resp;
            if (isEdit) {
                resp = await fetch(
                    '/api/themes/custom/' + encodeURIComponent(draft.id),
                    {
                        method: 'PUT',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify(payload),
                    }
                );
            } else {
                resp = await fetch('/api/themes/custom', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload),
                });
            }
            if (!resp.ok) {
                const text = await resp.text().catch(() => '');
                window.alert('Failed to save theme: ' + (text || resp.status));
                return;
            }
            close();
            const panel = document.getElementById('ps-panel-themes');
            if (panel) {
                const themesState = { loaded: false, data: null };
                loadThemesIntoPanel(panel, themesState);
            }
        } catch (e) {
            window.alert('Failed to save theme: ' + (e && e.message ? e.message : e));
        } finally {
            saveBtn.disabled = false;
            cancelBtn.disabled = false;
        }
    });
}

export function buildColorPickerRow(def, initialHex, onChange, compact) {
    const el = document.createElement('div');
    el.className = 'theme-editor-row theme-editor-color-row'
        + (compact ? ' theme-editor-color-row-compact' : '');

    const label = document.createElement('span');
    label.className = 'theme-editor-row-label';
    label.textContent = def.label;
    el.appendChild(label);

    const pair = document.createElement('div');
    pair.className = 'theme-editor-color-pair';

    const colorInput = document.createElement('input');
    colorInput.type = 'color';
    colorInput.className = 'theme-editor-color-input';
    colorInput.value = normalizeHex(initialHex, '#000000');

    const hexInput = document.createElement('input');
    hexInput.type = 'text';
    hexInput.className = 'theme-editor-hex-input';
    hexInput.maxLength = 7;
    hexInput.spellcheck = false;
    hexInput.value = colorInput.value;

    colorInput.addEventListener('input', () => {
        const v = colorInput.value.toLowerCase();
        hexInput.value = v;
        hexInput.classList.remove('invalid');
        onChange(v);
    });

    hexInput.addEventListener('input', () => {
        const v = hexInput.value.trim();
        if (HEX_COLOR_RE.test(v)) {
            hexInput.classList.remove('invalid');
            colorInput.value = v.toLowerCase();
            onChange(v.toLowerCase());
        } else {
            hexInput.classList.add('invalid');
        }
    });
    hexInput.addEventListener('blur', () => {
        if (!HEX_COLOR_RE.test(hexInput.value.trim())) {
            hexInput.value = colorInput.value;
            hexInput.classList.remove('invalid');
        }
    });

    pair.appendChild(colorInput);
    pair.appendChild(hexInput);
    el.appendChild(pair);

    return {
        el,
        setValue(hex) {
            const v = normalizeHex(hex, '#000000');
            colorInput.value = v;
            hexInput.value = v;
            hexInput.classList.remove('invalid');
        },
    };
}

export function buildLivePreviewContainer() {
    const el = document.createElement('div');
    el.className = 'theme-editor-preview';

    const uiBlock = document.createElement('div');
    uiBlock.className = 'theme-preview-ui';
    const side = document.createElement('div');
    side.className = 'theme-preview-sidebar';
    const sideTitle = document.createElement('div');
    sideTitle.className = 'theme-preview-sidebar-title';
    sideTitle.textContent = 'Sessions';
    side.appendChild(sideTitle);
    const sideList = document.createElement('ul');
    sideList.className = 'theme-preview-sidebar-list';
    ['main', 'logs', 'editor'].forEach((s, i) => {
        const li = document.createElement('li');
        li.className = 'theme-preview-sidebar-item' + (i === 0 ? ' active' : '');
        li.textContent = s;
        sideList.appendChild(li);
    });
    side.appendChild(sideList);
    uiBlock.appendChild(side);
    const main = document.createElement('div');
    main.className = 'theme-preview-main';
    const text = document.createElement('div');
    text.className = 'theme-preview-text';
    text.textContent = 'Sample text — primary foreground.';
    main.appendChild(text);
    const dim = document.createElement('div');
    dim.className = 'theme-preview-text-dim';
    dim.textContent = 'Dimmer secondary text — fg-dim.';
    main.appendChild(dim);
    const tags = document.createElement('div');
    tags.className = 'theme-preview-tags';
    ['p0', 'p1', 'p2'].forEach((p) => {
        const t = document.createElement('span');
        t.className = 'theme-preview-tag theme-preview-tag-' + p;
        t.textContent = p.toUpperCase();
        tags.appendChild(t);
    });
    main.appendChild(tags);
    const btnRow = document.createElement('div');
    btnRow.className = 'theme-preview-buttons';
    const btnAccent = document.createElement('button');
    btnAccent.className = 'theme-preview-btn theme-preview-btn-accent';
    btnAccent.type = 'button';
    btnAccent.textContent = 'Action';
    const btnWarn = document.createElement('button');
    btnWarn.className = 'theme-preview-btn theme-preview-btn-warn';
    btnWarn.type = 'button';
    btnWarn.textContent = 'Warn';
    const btnDanger = document.createElement('button');
    btnDanger.className = 'theme-preview-btn theme-preview-btn-danger';
    btnDanger.type = 'button';
    btnDanger.textContent = 'Danger';
    btnRow.appendChild(btnAccent);
    btnRow.appendChild(btnWarn);
    btnRow.appendChild(btnDanger);
    main.appendChild(btnRow);
    uiBlock.appendChild(main);
    el.appendChild(uiBlock);

    const term = document.createElement('div');
    term.className = 'theme-preview-term';
    const termLine1 = document.createElement('div');
    termLine1.textContent = '$ ls --color';
    term.appendChild(termLine1);
    const termLine2 = document.createElement('div');
    const base8 = ['black','red','green','yellow','blue','magenta','cyan','white'];
    const span8 = {};
    base8.forEach((k) => {
        const s = document.createElement('span');
        s.className = 'theme-preview-ansi';
        s.textContent = k + ' ';
        span8[k] = s;
        termLine2.appendChild(s);
    });
    term.appendChild(termLine2);
    const termLine3 = document.createElement('div');
    const bright8 = ['brightBlack','brightRed','brightGreen','brightYellow',
                    'brightBlue','brightMagenta','brightCyan','brightWhite'];
    const spanBright = {};
    bright8.forEach((k) => {
        const s = document.createElement('span');
        s.className = 'theme-preview-ansi';
        s.textContent = k.replace('bright', 'br.').toLowerCase() + ' ';
        spanBright[k] = s;
        termLine3.appendChild(s);
    });
    term.appendChild(termLine3);
    const termLine4 = document.createElement('div');
    const sel = document.createElement('span');
    sel.className = 'theme-preview-selection';
    sel.textContent = 'selected';
    const cur = document.createElement('span');
    cur.className = 'theme-preview-cursor';
    cur.textContent = '█';
    termLine4.appendChild(document.createTextNode('cursor '));
    termLine4.appendChild(cur);
    termLine4.appendChild(document.createTextNode(' selection '));
    termLine4.appendChild(sel);
    term.appendChild(termLine4);
    el.appendChild(term);

    function update(draft) {
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
        for (const [k, cssVar] of Object.entries(cssMap)) {
            const v = draft.ui[k];
            if (typeof v === 'string') {
                el.style.setProperty(cssVar, v);
            }
        }
        term.style.background = draft.term.background;
        term.style.color = draft.term.foreground;
        term.style.border = '1px solid ' + draft.term.foreground;
        for (const k of base8) {
            if (span8[k]) span8[k].style.color = draft.term[k];
        }
        for (const k of bright8) {
            if (spanBright[k]) spanBright[k].style.color = draft.term[k];
        }
        cur.style.background = draft.term.cursor;
        cur.style.color = draft.term.background;
        sel.style.background = draft.term.selection;
        sel.style.color = draft.term.foreground;
    }

    return { el, update };
}
