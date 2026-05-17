// tmux-web — Echo memory viewer/editor (Phase 5c)
//
// Три таба:
//   - global_day   — глобальные дневные summary
//   - project      — стабильная per-project memory
//   - project_day  — per-project дневные summary
//
// Базовые операции: list / patch (inline edit) / delete / regenerate.

import {
    listMemories, patchMemory, deleteMemory, regenerateMemory,
} from './api.js';
import {
    $echoMemoryList, $echoMemoryRegen, $echoMemory,
} from '../core/dom.js';
import { notify } from './notifications.js';

const SCOPES = ['global_day', 'project', 'project_day'];

let _bound = false;
let _scope = 'global_day';
let _projectId = null;
let _day = null;

export function initMemoryPane() {
    if (_bound) return;
    _bound = true;
    if ($echoMemory) {
        const tabs = $echoMemory.querySelectorAll('.echo-mem-tab');
        tabs.forEach((t) => {
            t.addEventListener('click', () => {
                tabs.forEach((x) => x.classList.remove('active'));
                t.classList.add('active');
                _scope = t.dataset.scope;
                refreshMemory();
            });
        });
    }
    if ($echoMemoryRegen) {
        $echoMemoryRegen.addEventListener('click', async () => {
            await runRegenerate();
        });
    }
    refreshMemory();
}

/** Обновить активный project / day для memory pane (вызывается из main.js
 *  при переключении project-filter). */
export function setMemoryFilters(projectId, day) {
    _projectId = projectId || null;
    _day = day || null;
    refreshMemory();
}

export async function refreshMemory() {
    if (!$echoMemoryList) return;
    const params = { scope: _scope };
    if (_scope !== 'global_day' && _projectId) params.projectId = _projectId;
    if (_scope !== 'project' && _day) params.day = _day;
    let data;
    try {
        data = await listMemories(params);
    } catch (e) {
        $echoMemoryList.innerHTML = `<li class="echo-empty">Ошибка: ${escapeHtml(e.message || e)}</li>`;
        return;
    }
    const items = (data && data.items) || [];
    if (items.length === 0) {
        $echoMemoryList.innerHTML = '<li class="echo-empty">Нет memories</li>';
        return;
    }
    $echoMemoryList.innerHTML = '';
    for (const m of items) {
        // Пропускаем служебный маркер (используется scheduler'ом).
        if (m.day === '__last_rollover__') continue;
        $echoMemoryList.appendChild(buildMemoryNode(m));
    }
}

function buildMemoryNode(m) {
    const li = document.createElement('li');
    li.className = 'echo-mem-item';
    li.dataset.id = m.id;

    const head = document.createElement('div');
    head.className = 'echo-mem-head';
    const title = document.createElement('div');
    title.className = 'echo-mem-title';
    title.textContent = formatHeading(m);
    head.appendChild(title);
    const src = document.createElement('span');
    src.className = 'echo-mem-source';
    src.textContent = m.source || 'manual';
    head.appendChild(src);
    li.appendChild(head);

    const content = document.createElement('pre');
    content.className = 'echo-mem-content';
    content.textContent = m.content || '';
    li.appendChild(content);

    const actions = document.createElement('div');
    actions.className = 'echo-mem-actions';
    actions.appendChild(makeBtn('Edit', () => beginEdit(li, m)));
    actions.appendChild(makeBtn('Delete', async () => {
        if (!confirm('Удалить memory?')) return;
        try {
            await deleteMemory(m.id);
            refreshMemory();
        } catch (e) {
            notify({ level: 'error', title: 'Delete failed', body: e.message });
        }
    }, 'danger'));
    li.appendChild(actions);
    return li;
}

function beginEdit(li, m) {
    const content = li.querySelector('.echo-mem-content');
    const ta = document.createElement('textarea');
    ta.className = 'echo-mem-edit';
    ta.value = m.content || '';
    ta.rows = Math.min(20, (m.content || '').split('\n').length + 1);
    li.replaceChild(ta, content);
    const actions = li.querySelector('.echo-mem-actions');
    actions.innerHTML = '';
    actions.appendChild(makeBtn('Save', async () => {
        try {
            await patchMemory(m.id, ta.value);
            refreshMemory();
        } catch (e) {
            notify({ level: 'error', title: 'Save failed', body: e.message });
        }
    }, 'primary'));
    actions.appendChild(makeBtn('Cancel', () => refreshMemory()));
}

function formatHeading(m) {
    const scope = m.scope || '';
    const proj = m.project_id ? ` · ${m.project_id}` : '';
    const day = m.day ? ` · ${m.day}` : '';
    return `${scope}${proj}${day}`;
}

async function runRegenerate() {
    const payload = { scope: _scope };
    if (_scope !== 'global_day') {
        if (!_projectId) {
            notify({
                level: 'warn',
                title: 'Regenerate',
                body: 'Выберите project_id для этого scope',
            });
            return;
        }
        payload.projectId = _projectId;
    }
    if (_scope !== 'project') {
        if (!_day) {
            notify({
                level: 'warn',
                title: 'Regenerate',
                body: 'Выберите day (YYYY-MM-DD) для этого scope',
            });
            return;
        }
        payload.day = _day;
    }
    notify({ level: 'info', title: 'Regenerating…', body: `scope=${_scope}` });
    try {
        await regenerateMemory(payload);
        notify({ level: 'info', title: 'Regenerated', body: payload.scope });
        refreshMemory();
    } catch (e) {
        notify({ level: 'error', title: 'Regenerate failed', body: e.message });
    }
}

function makeBtn(label, fn, cls) {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.className = 'echo-mem-btn' + (cls ? ' echo-mem-btn-' + cls : '');
    b.addEventListener('click', fn);
    return b;
}

function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({
        '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    })[c]);
}

// Re-export для удобства callers, которые хотят узнать список scopes
// (например, для tooltips).
export { SCOPES };
