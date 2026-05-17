// tmux-web — Tasks modals (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - TASK_EDIT_STATUSES / TASK_TYPES (app.js:5597, 5602)
//   - buildTaskFormHtml (app.js:5823)
//   - openCreateModal   (app.js:5864)
//   - openEditModal     (app.js:5972)
//   - openTodoEditModal (app.js:6073)

import { state } from '../core/state.js';
import { apiFetch, dtoOrigin } from '../core/api.js';
import {
    buildModalOverlay,
    escapeAttr,
    escapeText,
} from '../core/utils.js';
import { COLUMN_TITLES } from './render.js';
import { createTask, updateTask, closeTask, reopenTask, promoteTodo } from './crud.js';
import { fetchTodos } from '../ws/todos-ws.js';

const TASK_EDIT_STATUSES = ['open', 'in_progress', 'blocked', 'deferred', 'draft', 'closed'];
const TASK_TYPES = ['task', 'bug', 'feature', 'epic', 'chore', 'docs', 'question'];

export function buildTaskFormHtml(initial, isEdit) {
    const i = initial || {};
    const sel = (val, current) => (String(val) === String(current) ? ' selected' : '');
    const statusOptions = TASK_EDIT_STATUSES
        .map((s) => `<option value="${s}"${sel(s, i.status || 'open')}>${COLUMN_TITLES[s] || s}</option>`).join('');
    const typeOptions = TASK_TYPES
        .map((t) => `<option value="${t}"${sel(t, i.issue_type || 'task')}>${t}</option>`).join('');
    const prioOptions = [0, 1, 2, 3, 4]
        .map((p) => `<option value="${p}"${sel(p, (typeof i.priority === 'number') ? i.priority : 2)}>P${p}</option>`).join('');

    const labelsCsv = Array.isArray(i.labels) ? i.labels.join(',') : (i.labels || '');
    const idLine = isEdit && i.id ? `<div class="modal-id">${i.id}</div>` : '';
    const statusBlock = isEdit
        ? `<label>Status<br><select id="tm-status">${statusOptions}</select></label>`
        : '';

    return `
        ${idLine}
        <label>Title<br><input type="text" id="tm-title" value="${escapeAttr(i.title || '')}" placeholder="Краткое описание"></label>
        <label>Description<br><textarea id="tm-desc" placeholder="Подробности (опционально)">${escapeText(i.description || '')}</textarea></label>
        <div class="field-row">
            <label>Priority<br><select id="tm-prio">${prioOptions}</select></label>
            <label>Type<br><select id="tm-type">${typeOptions}</select></label>
            ${statusBlock || '<label>&nbsp;<br><span style="opacity:0.5;font-size:0.7rem">status выставится open</span></label>'}
        </div>
        <label>Labels (csv)<br><input type="text" id="tm-labels" value="${escapeAttr(labelsCsv)}" placeholder="phase-6,api"></label>
    `;
}

export function openCreateModal(preset) {
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card task-modal';

    const initial = Object.assign({ status: 'open' }, preset || {});
    const isTodo = initial.status === 'todo';
    if (isTodo) {
        const us = state.userSettings || null;
        if (initial.priority === undefined || initial.priority === null) {
            initial.priority = (us && typeof us.todo_default_priority === 'number')
                ? us.todo_default_priority
                : 2;
        }
        if (!initial.issue_type) {
            initial.issue_type = (us && us.todo_default_issue_type)
                ? us.todo_default_issue_type
                : 'task';
        }
    }
    const heading = isTodo ? 'New TODO' : 'New task';
    const todoPlanDefault = isTodo
        ? !!(state.userSettings && state.userSettings.todo_default_plan_mode)
        : false;
    const planModeBlock = isTodo
        ? `<label class="checkbox-row"><input type="checkbox" id="tm-plan-mode"${todoPlanDefault ? ' checked' : ''}> Включить план мод
             <span class="hint">— при promote добавит «Создай план для этой задачи»</span></label>`
        : '';
    card.innerHTML = `
        <h2>${heading}</h2>
        ${buildTaskFormHtml(initial, false)}
        ${planModeBlock}
        <div class="modal-actions">
            <button type="button" id="tm-cancel">Cancel</button>
            <button type="button" id="tm-save" class="primary">Create</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $title = card.querySelector('#tm-title');
    const $desc = card.querySelector('#tm-desc');
    const $prio = card.querySelector('#tm-prio');
    const $type = card.querySelector('#tm-type');
    const $labels = card.querySelector('#tm-labels');
    $title.focus();

    const close = () => overlay.remove();
    card.querySelector('#tm-cancel').addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });
    card.querySelector('#tm-save').addEventListener('click', async () => {
        const title = ($title.value || '').trim();
        if (!title) {
            window.alert('Title обязателен');
            return;
        }

        if (isTodo) {
            const projectId = state.activeProjectId;
            if (!projectId) {
                window.alert('Активный проект не выбран');
                return;
            }
            const $planMode = card.querySelector('#tm-plan-mode');
            const todoPayload = {
                project_id: projectId,
                title,
                description: ($desc.value || '').trim() || undefined,
                plan_mode: $planMode ? !!$planMode.checked : false,
            };
            close();
            try {
                const r = await fetch('/api/todos', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(todoPayload),
                });
                if (!r.ok) {
                    const text = await r.text();
                    window.alert('Создание TODO не удалось: ' + (text || r.status));
                    return;
                }
                setTimeout(() => {
                    if (!state.todosWs || state.todosWs.readyState !== WebSocket.OPEN) {
                        fetchTodos();
                    }
                }, 200);
            } catch (e) {
                window.alert('Ошибка запроса: ' + e.message);
            }
            return;
        }

        const payload = {
            title,
            description: ($desc.value || '').trim() || undefined,
            type: $type.value,
            priority: parseInt($prio.value, 10),
            labels: ($labels.value || '').trim() || undefined,
        };
        const wantStatus = initial.status && initial.status !== 'open' ? initial.status : null;
        close();
        const created = await createTask(Object.assign({}, payload, wantStatus ? { status: wantStatus } : {}));
        if (created && wantStatus && created.status !== wantStatus) {
            await updateTask(created.id, { status: wantStatus });
        }
    });
}

export function openEditModal(issue) {
    if (!issue || !issue.id) return;
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card task-modal';

    const isClosed = String(issue.status || '').toLowerCase() === 'closed';
    const reopenBtn = isClosed
        ? `<button type="button" id="tm-reopen" class="warn">Reopen</button>`
        : `<button type="button" id="tm-close-task" class="warn">Close…</button>`;

    card.innerHTML = `
        <h2>Edit task</h2>
        ${buildTaskFormHtml(issue, true)}
        <div class="modal-actions">
            ${reopenBtn}
            <span class="spacer"></span>
            <button type="button" id="tm-cancel">Cancel</button>
            <button type="button" id="tm-save" class="primary">Save</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $title = card.querySelector('#tm-title');
    const $desc = card.querySelector('#tm-desc');
    const $prio = card.querySelector('#tm-prio');
    const $type = card.querySelector('#tm-type');
    const $status = card.querySelector('#tm-status');
    const $labels = card.querySelector('#tm-labels');

    const close = () => overlay.remove();
    card.querySelector('#tm-cancel').addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });

    card.querySelector('#tm-save').addEventListener('click', async () => {
        const newTitle = ($title.value || '').trim();
        if (!newTitle) {
            window.alert('Title обязателен');
            return;
        }
        const patch = {};
        if (newTitle !== (issue.title || '')) patch.title = newTitle;
        const newDesc = ($desc.value || '');
        if (newDesc !== (issue.description || '')) patch.description = newDesc;
        const newPrio = parseInt($prio.value, 10);
        if (Number.isFinite(newPrio) && newPrio !== issue.priority) patch.priority = newPrio;
        const newStatus = $status.value;
        if (newStatus !== (issue.status || '')) patch.status = newStatus;
        const newType = $type.value;
        if (newType !== (issue.issue_type || '')) {
            // br update --type существует, но мы в API не маппили — пропускаем.
        }
        const newLabels = ($labels.value || '').trim();
        const oldLabelsCsv = Array.isArray(issue.labels) ? issue.labels.join(',') : (issue.labels || '');
        if (newLabels !== oldLabelsCsv) patch.labels = newLabels;

        if (Object.keys(patch).length === 0) {
            close();
            return;
        }
        close();
        await updateTask(issue.id, patch);
    });

    const $closeTask = card.querySelector('#tm-close-task');
    if ($closeTask) {
        $closeTask.addEventListener('click', async () => {
            const reason = window.prompt('Причина закрытия задачи:', '') || '';
            close();
            await closeTask(issue.id, reason.trim() || undefined);
        });
    }

    const $reopen = card.querySelector('#tm-reopen');
    if ($reopen) {
        $reopen.addEventListener('click', async () => {
            close();
            await reopenTask(issue.id);
        });
    }
}

export function openTodoEditModal(todo) {
    if (!todo || !todo.id) return;
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card task-modal';

    let defaultSession = state.currentSession || '';
    if (!defaultSession) {
        const projectId = todo.project_id || state.activeProjectId || null;
        const projectSessions = (state.sessions || [])
            .filter((s) => projectId ? s.project_id === projectId : true)
            .map((s) => s.name)
            .sort((a, b) => String(a).localeCompare(String(b)));
        if (projectSessions.length > 0) defaultSession = projectSessions[0];
    }

    const planChecked = todo.plan_mode ? ' checked' : '';
    card.innerHTML = `
        <h2>Edit TODO</h2>
        <div class="modal-id">${escapeText(todo.id)}</div>
        <label>Title<br><input type="text" id="td-title" value="${escapeAttr(todo.title || '')}" placeholder="Краткое описание"></label>
        <label>Description<br><textarea id="td-desc" placeholder="Подробности (опционально)">${escapeText(todo.description || '')}</textarea></label>
        <label class="checkbox-row"><input type="checkbox" id="td-plan-mode"${planChecked}> Включить план мод
            <span class="hint">— при promote добавит «Создай план для этой задачи»</span></label>
        <label>Promote → tmux session<br><input type="text" id="td-session" value="${escapeAttr(defaultSession)}" placeholder="${escapeAttr(defaultSession || 'session name')}"></label>
        <div class="modal-actions">
            <button type="button" id="td-delete" class="warn">Delete</button>
            <span class="spacer"></span>
            <button type="button" id="td-cancel">Cancel</button>
            <button type="button" id="td-promote" class="primary">Promote</button>
            <button type="button" id="td-save" class="primary">Save</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $title = card.querySelector('#td-title');
    const $desc = card.querySelector('#td-desc');
    const $session = card.querySelector('#td-session');
    const $planMode = card.querySelector('#td-plan-mode');
    $title.focus();

    const close = () => overlay.remove();
    card.querySelector('#td-cancel').addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });

    card.querySelector('#td-save').addEventListener('click', async () => {
        const newTitle = ($title.value || '').trim();
        if (!newTitle) {
            window.alert('Title обязателен');
            return;
        }
        const patch = {};
        if (newTitle !== (todo.title || '')) patch.title = newTitle;
        const newDesc = ($desc.value || '');
        if (newDesc !== (todo.description || '')) patch.description = newDesc;
        const newPlanMode = $planMode ? !!$planMode.checked : false;
        if (newPlanMode !== !!todo.plan_mode) patch.plan_mode = newPlanMode;
        if (Object.keys(patch).length === 0) {
            close();
            return;
        }
        close();
        try {
            const origin = dtoOrigin(todo);
            const r = await apiFetch('/api/todos/' + encodeURIComponent(todo.id), {
                method: 'PATCH',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(patch),
            }, origin);
            if (!r.ok) {
                const text = await r.text();
                window.alert('Update не удался: ' + (text || r.status));
                return;
            }
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
        }
    });

    card.querySelector('#td-delete').addEventListener('click', async () => {
        const confirmDelete = !(state.userSettings && state.userSettings.todo_confirm_delete === false);
        if (confirmDelete && !window.confirm('Удалить TODO?')) return;
        close();
        try {
            const origin = dtoOrigin(todo);
            const r = await apiFetch('/api/todos/' + encodeURIComponent(todo.id), {
                method: 'DELETE',
            }, origin);
            if (!r.ok && r.status !== 204) {
                const text = await r.text();
                window.alert('Delete не удался: ' + (text || r.status));
                return;
            }
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
        }
    });

    card.querySelector('#td-promote').addEventListener('click', async () => {
        const sessionVal = ($session.value || '').trim() || undefined;
        close();
        await promoteTodo(todo.id, sessionVal);
    });
}
