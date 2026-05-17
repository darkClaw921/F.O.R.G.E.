// tmux-web — Echo autonomous tasks UI (Phase 5c)
//
// CRUD-UI поверх /api/echo/autonomous-tasks. Рендерит список с toggle
// (enabled/disabled), кнопками Run now / Edit / Delete + история runs
// под выбранной задачей.
//
// Use:
//   import { initAutonomousPane, refreshAutonomous } from './autonomous.js';
//   initAutonomousPane();      // вешает кнопку "+ New" + первичный рендер
//   refreshAutonomous();       // пересчитать список (из ws-event)

import {
    listAutonomousTasks, createAutonomousTask, patchAutonomousTask,
    deleteAutonomousTask, runAutonomousNow, listAutonomousRuns,
} from './api.js';
import {
    $echoAutonomousList, $echoNewAuto,
} from '../core/dom.js';
import { notify } from './notifications.js';

const INTERVAL_PRESETS = [
    { label: '1 min', secs: 60 },
    { label: '5 min', secs: 300 },
    { label: '15 min', secs: 900 },
    { label: '1 hr', secs: 3600 },
    { label: '6 hr', secs: 21600 },
    { label: '24 hr', secs: 86400 },
];

let _bound = false;

export function initAutonomousPane() {
    if (_bound) return;
    _bound = true;
    if ($echoNewAuto) {
        $echoNewAuto.addEventListener('click', () => openCreateModal());
    }
    refreshAutonomous();
}

export async function refreshAutonomous() {
    if (!$echoAutonomousList) return;
    let data;
    try {
        data = await listAutonomousTasks();
    } catch (e) {
        $echoAutonomousList.innerHTML = `<li class="echo-empty">Ошибка: ${escapeHtml(e.message || e)}</li>`;
        return;
    }
    const items = (data && data.items) || [];
    if (items.length === 0) {
        $echoAutonomousList.innerHTML = '<li class="echo-empty">Нет автозадач</li>';
        return;
    }
    $echoAutonomousList.innerHTML = '';
    for (const t of items) {
        $echoAutonomousList.appendChild(buildTaskNode(t));
    }
}

function buildTaskNode(t) {
    const li = document.createElement('li');
    li.className = 'echo-auto-item';
    li.dataset.id = t.id;

    const head = document.createElement('div');
    head.className = 'echo-auto-head';

    const toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.checked = !!t.enabled;
    toggle.title = t.enabled ? 'Отключить' : 'Включить';
    toggle.addEventListener('change', async () => {
        try {
            await patchAutonomousTask(t.id, { enabled: toggle.checked });
            t.enabled = toggle.checked;
        } catch (e) {
            notify({ level: 'error', title: 'Toggle failed', body: e.message });
            toggle.checked = !toggle.checked;
        }
    });
    head.appendChild(toggle);

    const name = document.createElement('div');
    name.className = 'echo-auto-name';
    name.textContent = t.name;
    head.appendChild(name);

    const meta = document.createElement('div');
    meta.className = 'echo-auto-meta';
    meta.textContent = `${formatInterval(t.interval_seconds)} · ${t.model || 'sonnet'}`;
    head.appendChild(meta);

    li.appendChild(head);

    const actions = document.createElement('div');
    actions.className = 'echo-auto-actions';
    actions.appendChild(makeBtn('Run', async () => {
        try {
            await runAutonomousNow(t.id);
            notify({ level: 'info', title: 'Run scheduled', body: t.name });
        } catch (e) {
            notify({ level: 'error', title: 'Run failed', body: e.message });
        }
    }));
    actions.appendChild(makeBtn('Runs', async () => {
        await openRunsModal(t);
    }));
    actions.appendChild(makeBtn('Edit', () => openEditModal(t)));
    actions.appendChild(makeBtn('Delete', async () => {
        if (!confirm(`Удалить «${t.name}»?`)) return;
        try {
            await deleteAutonomousTask(t.id);
            refreshAutonomous();
        } catch (e) {
            notify({ level: 'error', title: 'Delete failed', body: e.message });
        }
    }, 'danger'));
    li.appendChild(actions);
    return li;
}

function makeBtn(label, fn, cls) {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.className = 'echo-auto-btn' + (cls ? ' echo-auto-btn-' + cls : '');
    b.addEventListener('click', fn);
    return b;
}

function formatInterval(secs) {
    if (!secs || secs < 60) return `${secs || 0}s`;
    if (secs < 3600) return `${Math.round(secs / 60)}m`;
    if (secs < 86400) return `${Math.round(secs / 3600)}h`;
    return `${Math.round(secs / 86400)}d`;
}

function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({
        '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    })[c]);
}

// -------- modals (минимально-инвазивные оверлеи) --------

function openCreateModal() {
    openTaskModal({
        title: 'Новая автозадача',
        initial: {
            name: '',
            prompt_template: '',
            interval_seconds: 3600,
            model: 'claude-3-5-sonnet-latest',
            project_id: null,
        },
        onSubmit: async (payload) => {
            await createAutonomousTask(payload);
            refreshAutonomous();
        },
    });
}

function openEditModal(t) {
    openTaskModal({
        title: 'Изменить автозадачу',
        initial: t,
        onSubmit: async (payload) => {
            await patchAutonomousTask(t.id, payload);
            refreshAutonomous();
        },
    });
}

function openTaskModal({ title, initial, onSubmit }) {
    const overlay = document.createElement('div');
    overlay.className = 'echo-modal-overlay';
    const modal = document.createElement('div');
    modal.className = 'echo-modal';
    overlay.appendChild(modal);

    const h = document.createElement('h3');
    h.textContent = title;
    modal.appendChild(h);

    const form = document.createElement('form');
    form.className = 'echo-modal-form';
    modal.appendChild(form);

    form.appendChild(labelInput('Название', 'text', 'name', initial.name || ''));
    form.appendChild(labelTextarea('Prompt template', 'prompt_template', initial.prompt_template || ''));
    form.appendChild(labelSelect('Интервал', 'interval_seconds', INTERVAL_PRESETS.map(p => ({
        value: p.secs, label: p.label,
    })), initial.interval_seconds));
    form.appendChild(labelInput('Model', 'text', 'model', initial.model || 'claude-3-5-sonnet-latest'));
    form.appendChild(labelInput('Project ID (optional)', 'text', 'project_id', initial.project_id || ''));

    const buttons = document.createElement('div');
    buttons.className = 'echo-modal-buttons';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', () => overlay.remove());
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.textContent = 'Save';
    submit.className = 'primary';
    buttons.appendChild(cancel);
    buttons.appendChild(submit);
    form.appendChild(buttons);

    form.addEventListener('submit', async (ev) => {
        ev.preventDefault();
        const fd = new FormData(form);
        const payload = {
            name: fd.get('name'),
            prompt_template: fd.get('prompt_template'),
            interval_seconds: parseInt(fd.get('interval_seconds'), 10),
            model: fd.get('model'),
            project_id: fd.get('project_id') || null,
        };
        try {
            await onSubmit(payload);
            overlay.remove();
        } catch (e) {
            notify({ level: 'error', title: 'Save failed', body: e.message });
        }
    });

    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) overlay.remove();
    });
    document.body.appendChild(overlay);
}

async function openRunsModal(t) {
    const overlay = document.createElement('div');
    overlay.className = 'echo-modal-overlay';
    const modal = document.createElement('div');
    modal.className = 'echo-modal echo-modal-wide';
    overlay.appendChild(modal);
    const h = document.createElement('h3');
    h.textContent = `История: ${t.name}`;
    modal.appendChild(h);
    const tbl = document.createElement('table');
    tbl.className = 'echo-runs-table';
    tbl.innerHTML = '<thead><tr><th>Started</th><th>Status</th><th>Tokens</th><th>Error</th></tr></thead><tbody></tbody>';
    modal.appendChild(tbl);
    let data;
    try {
        data = await listAutonomousRuns(t.id, 50);
    } catch (e) {
        modal.appendChild(buildErrorBlock(e.message || e));
        document.body.appendChild(overlay);
        attachOverlayClose(overlay);
        return;
    }
    const tbody = tbl.querySelector('tbody');
    for (const r of (data.items || [])) {
        const tr = document.createElement('tr');
        const started = new Date((r.started_at || 0) * 1000).toLocaleString();
        const tokens = `${r.tokens_in || 0}/${r.tokens_out || 0}`;
        tr.innerHTML = `<td>${escapeHtml(started)}</td><td>${escapeHtml(r.status || '')}</td><td>${tokens}</td><td>${escapeHtml(r.error || '')}</td>`;
        tbody.appendChild(tr);
    }
    const close = document.createElement('button');
    close.type = 'button';
    close.textContent = 'Close';
    close.className = 'primary';
    close.addEventListener('click', () => overlay.remove());
    modal.appendChild(close);
    document.body.appendChild(overlay);
    attachOverlayClose(overlay);
}

function attachOverlayClose(overlay) {
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) overlay.remove();
    });
}

function buildErrorBlock(msg) {
    const d = document.createElement('div');
    d.className = 'echo-error-block';
    d.textContent = msg;
    return d;
}

function labelInput(label, type, name, value) {
    const wrap = document.createElement('label');
    wrap.className = 'echo-field';
    const span = document.createElement('span');
    span.textContent = label;
    const input = document.createElement('input');
    input.type = type;
    input.name = name;
    input.value = value || '';
    wrap.appendChild(span);
    wrap.appendChild(input);
    return wrap;
}

function labelTextarea(label, name, value) {
    const wrap = document.createElement('label');
    wrap.className = 'echo-field';
    const span = document.createElement('span');
    span.textContent = label;
    const ta = document.createElement('textarea');
    ta.name = name;
    ta.rows = 4;
    ta.value = value || '';
    wrap.appendChild(span);
    wrap.appendChild(ta);
    return wrap;
}

function labelSelect(label, name, opts, selected) {
    const wrap = document.createElement('label');
    wrap.className = 'echo-field';
    const span = document.createElement('span');
    span.textContent = label;
    const sel = document.createElement('select');
    sel.name = name;
    for (const o of opts) {
        const op = document.createElement('option');
        op.value = o.value;
        op.textContent = o.label;
        if (String(o.value) === String(selected)) op.selected = true;
        sel.appendChild(op);
    }
    wrap.appendChild(span);
    wrap.appendChild(sel);
    return wrap;
}
