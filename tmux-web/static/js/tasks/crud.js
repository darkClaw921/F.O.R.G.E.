// tmux-web — Tasks CRUD + optimistic UI (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - getIssueIndex / applyOptimisticPatch / rollbackIssue (app.js:5615-5639)
//   - createTask        (app.js:5647)
//   - updateTask        (app.js:5709)
//   - taskOriginById    (app.js:5757)
//   - closeTask         (app.js:5769)
//   - reopenTask        (app.js:5795)
//   - promoteTodo       (app.js:3516)

import { state } from '../core/state.js';
import { apiFetch, dtoOrigin } from '../core/api.js';
import { renderTasks } from './render.js';
import { fetchTasks } from '../ws/tasks-ws.js';

export function getIssueIndex(id) {
    if (!state.tasksData || !Array.isArray(state.tasksData.issues)) return -1;
    return state.tasksData.issues.findIndex((it) => it && it.id === id);
}

export function applyOptimisticPatch(id, patch) {
    const idx = getIssueIndex(id);
    if (idx < 0) return null;
    const prev = state.tasksData.issues[idx];
    const next = Object.assign({}, prev, patch);
    state.tasksData.issues[idx] = next;
    renderTasks();
    return prev;
}

export function rollbackIssue(id, prev) {
    if (!prev) return;
    const idx = getIssueIndex(id);
    if (idx < 0) {
        state.tasksData.issues.unshift(prev);
    } else {
        state.tasksData.issues[idx] = prev;
    }
    renderTasks();
}

export async function createTask(payload) {
    const tempId = 'tmp-' + Math.random().toString(36).slice(2, 8);
    const optimistic = {
        id: tempId,
        title: payload.title,
        description: payload.description || '',
        issue_type: payload.type || 'task',
        priority: (typeof payload.priority === 'number') ? payload.priority : 2,
        status: payload.status || 'open',
        labels: (payload.labels || '').split(',').map((s) => s.trim()).filter(Boolean),
        updated_at: new Date().toISOString(),
        __optimistic: true,
    };
    if (state.tasksData && Array.isArray(state.tasksData.issues)) {
        state.tasksData.issues.unshift(optimistic);
        renderTasks();
    }

    try {
        const r = await fetch('/api/tasks', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        });
        if (!r.ok) {
            const text = await r.text();
            window.alert('Создание не удалось: ' + (text || r.status));
            if (state.tasksData) {
                state.tasksData.issues = state.tasksData.issues.filter((it) => it.id !== tempId);
                renderTasks();
            }
            return null;
        }
        const created = await r.json();
        if (state.tasksData) {
            const idx = getIssueIndex(tempId);
            if (idx >= 0) {
                state.tasksData.issues[idx] = created;
            } else {
                state.tasksData.issues.unshift(created);
            }
            renderTasks();
        } else {
            fetchTasks();
        }
        return created;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        if (state.tasksData) {
            state.tasksData.issues = state.tasksData.issues.filter((it) => it.id !== tempId);
            renderTasks();
        }
        return null;
    }
}

export async function updateTask(id, payload) {
    const optimisticPatch = {};
    if ('status' in payload) optimisticPatch.status = payload.status;
    if ('title' in payload) optimisticPatch.title = payload.title;
    if ('priority' in payload) optimisticPatch.priority = payload.priority;
    if ('description' in payload) optimisticPatch.description = payload.description;
    if ('labels' in payload) {
        optimisticPatch.labels = (payload.labels || '').split(',').map((s) => s.trim()).filter(Boolean);
    }
    const prev = applyOptimisticPatch(id, optimisticPatch);

    try {
        const origin = taskOriginById(id);
        const r = await apiFetch('/api/tasks/' + encodeURIComponent(id), {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        }, origin);
        if (!r.ok) {
            const text = await r.text();
            window.alert('Update не удался: ' + (text || r.status));
            rollbackIssue(id, prev);
            return null;
        }
        const updatedArr = await r.json();
        const updated = Array.isArray(updatedArr) ? updatedArr.find((u) => u && u.id === id) : null;
        if (updated && state.tasksData) {
            const idx = getIssueIndex(id);
            if (idx >= 0) {
                state.tasksData.issues[idx] = Object.assign({}, state.tasksData.issues[idx], updated);
                renderTasks();
            }
        }
        return updated;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        rollbackIssue(id, prev);
        return null;
    }
}

export function taskOriginById(id) {
    if (!state.tasksData || !Array.isArray(state.tasksData.issues)) return 'local';
    const issue = state.tasksData.issues.find((it) => it && it.id === id);
    return dtoOrigin(issue);
}

export async function closeTask(id, reason) {
    const prev = applyOptimisticPatch(id, { status: 'closed' });
    try {
        const origin = taskOriginById(id);
        let url = '/api/tasks/' + encodeURIComponent(id)
            + (reason ? ('?reason=' + encodeURIComponent(reason)) : '');
        const r = await apiFetch(url, { method: 'DELETE' }, origin);
        if (!r.ok && r.status !== 204) {
            const text = await r.text();
            window.alert('Close не удался: ' + (text || r.status));
            rollbackIssue(id, prev);
            return false;
        }
        return true;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        rollbackIssue(id, prev);
        return false;
    }
}

export async function purgeTask(id) {
    const idx = getIssueIndex(id);
    const prev = (idx >= 0) ? state.tasksData.issues[idx] : null;
    if (idx >= 0) {
        state.tasksData.issues.splice(idx, 1);
        renderTasks();
    }
    try {
        const origin = taskOriginById(id);
        const r = await apiFetch('/api/tasks/' + encodeURIComponent(id) + '/purge', {
            method: 'POST',
        }, origin);
        if (!r.ok && r.status !== 204) {
            const text = await r.text();
            window.alert('Purge не удался: ' + (text || r.status));
            if (prev && state.tasksData) {
                state.tasksData.issues.splice(idx >= 0 ? idx : 0, 0, prev);
                renderTasks();
            }
            return false;
        }
        return true;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        if (prev && state.tasksData) {
            state.tasksData.issues.splice(idx >= 0 ? idx : 0, 0, prev);
            renderTasks();
        }
        return false;
    }
}

export async function deleteTodoLocal(id) {
    const idx = Array.isArray(state.todosData)
        ? state.todosData.findIndex((t) => t && t.id === id)
        : -1;
    const prev = (idx >= 0) ? state.todosData[idx] : null;
    if (idx >= 0) {
        state.todosData.splice(idx, 1);
        renderTasks();
    }
    try {
        const origin = dtoOrigin(prev) || 'local';
        const r = await apiFetch('/api/todos/' + encodeURIComponent(id), {
            method: 'DELETE',
        }, origin);
        if (!r.ok && r.status !== 204) {
            const text = await r.text();
            window.alert('Delete TODO не удался: ' + (text || r.status));
            if (prev) {
                state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
                renderTasks();
            }
            return false;
        }
        return true;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        if (prev) {
            state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
            renderTasks();
        }
        return false;
    }
}

export async function cleanColumn(status, ids) {
    if (!Array.isArray(ids) || ids.length === 0) return { ok: 0, fail: 0 };
    let ok = 0;
    let fail = 0;
    for (const id of ids) {
        let success = false;
        if (status === 'closed') {
            success = await purgeTask(id);
        } else if (status === 'todo') {
            success = await deleteTodoLocal(id);
        } else {
            success = await closeTask(id, 'clean-column');
        }
        if (success) ok += 1; else fail += 1;
    }
    return { ok, fail };
}

export async function reopenTask(id) {
    const prev = applyOptimisticPatch(id, { status: 'open' });
    try {
        const origin = taskOriginById(id);
        const r = await apiFetch('/api/tasks/' + encodeURIComponent(id) + '/reopen', {
            method: 'POST',
        }, origin);
        if (!r.ok && r.status !== 204) {
            const text = await r.text();
            window.alert('Reopen не удался: ' + (text || r.status));
            rollbackIssue(id, prev);
            return false;
        }
        return true;
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        rollbackIssue(id, prev);
        return false;
    }
}

export async function promoteTodo(id, sessionOverride) {
    if (!id) return;

    const idx = Array.isArray(state.todosData)
        ? state.todosData.findIndex((t) => t && t.id === id)
        : -1;
    const prev = (idx >= 0) ? state.todosData[idx] : null;

    let session = sessionOverride && String(sessionOverride).trim()
        ? String(sessionOverride).trim()
        : (state.currentSession || null);
    if (!session) {
        const projectId = prev && prev.project_id
            ? prev.project_id
            : (state.activeProjectId || null);
        const projectSessions = (state.sessions || [])
            .filter((s) => projectId ? s.project_id === projectId : true)
            .map((s) => s.name)
            .sort((a, b) => String(a).localeCompare(String(b)));
        if (projectSessions.length > 0) {
            session = projectSessions[0];
        }
    }
    if (!session) {
        window.alert('Нет активной сессии для уведомления. Открой/создай tmux-сессию для проекта.');
        return;
    }

    if (idx >= 0) {
        state.todosData.splice(idx, 1);
        renderTasks();
    }

    try {
        const origin = dtoOrigin(prev) || 'local';
        const r = await apiFetch('/api/todos/' + encodeURIComponent(id) + '/promote', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ session }),
        }, origin);
        if (!r.ok) {
            const text = await r.text();
            window.alert('Promote не удался: ' + (text || r.status));
            if (prev) {
                state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
                renderTasks();
            }
            return null;
        }
        return await r.json();
    } catch (e) {
        window.alert('Ошибка запроса: ' + e.message);
        if (prev) {
            state.todosData.splice(idx >= 0 ? idx : 0, 0, prev);
            renderTasks();
        }
        return null;
    }
}
